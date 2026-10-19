import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { ImageAttachmentDrafts } from "../src/codex-workspace/attachments.js";
import { GatewayApi, GatewayError } from "../src/codex-workspace/api.js";
import { RecordStore } from "../src/codex-ui/record-store.js";
import { freshWorkspace } from "../src/codex-workspace/model.js";

const nodes = new Map();
function node(id) {
  if (!nodes.has(id)) nodes.set(id, { value: "", textContent: "", disabled: false, hidden: false, style: {}, scrollHeight: 28, classList: { add() {}, remove() {}, toggle() {} }, setAttribute() {}, querySelector: () => null });
  return nodes.get(id);
}
globalThis.localStorage = { getItem: () => null };
globalThis.matchMedia = () => ({ matches: false });
globalThis.location = { href: "http://127.0.0.1/codex-workspace" };
globalThis.history = { replaceState() {} };
globalThis.document = { body: { dataset: {} }, getElementById: node };
const source = await readFile(new URL("../src/codex-workspace/monitor-controls.js", import.meta.url), "utf8");
// Preserve the controller's exact methods; only browser-root module URLs need Node file URL resolution.
const resolveBrowserImports = source => source.replace(/from "\.\/([^"\n]+)"/g, (_, path) => `from "${new URL(`../src/codex-workspace/${path}`, import.meta.url)}"`)
  .replace(/from "\/monitor-ui\/([^"\n]+)"/g, (_, path) => `from "${new URL(`../src/codex-ui/${path}`, import.meta.url)}"`);
const temporary = await mkdtemp(join(tmpdir(), "monitor-controller-test-"));
const pickerPath = pathToFileURL(join(temporary, "project-picker.mjs"));
const pickerSource = await readFile(new URL("../src/codex-workspace/project-picker.js", import.meta.url), "utf8");
await writeFile(pickerPath, resolveBrowserImports(pickerSource));
const resolved = resolveBrowserImports(source).replace(new URL("../src/codex-workspace/project-picker.js", import.meta.url).href, pickerPath.href);
await writeFile(join(temporary, "monitor-controls.mjs"), resolved);
const { MonitorControls } = await import(pathToFileURL(join(temporary, "monitor-controls.mjs")));
await rm(temporary, { recursive: true });
const { indexState } = await import("../src/codex-ui/state.js");

test("occupied-session retry is coalesced, keeps the draft, and sends no message", async () => {
  nodes.clear(); node("composeInput").value = "Keep my unsent draft";
  let release, reads = 0, paints = 0;
  const attachments = new ImageAttachmentDrafts();
  attachments.threads.set("a", [{ id: 1, status: "ready", url: "data:image/png;base64,test" }]);
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", pending: false, creating: false, attachments,
    state: { thread: { id: "a" }, connection: "connected", needsSnapshot: false, accessMode: "active" },
    latestError: { code: "thread_in_use", message: "Owned elsewhere" },
    paintBlockingState() {}, paint() { paints++; },
    recovery: { recover(options) { assert.deepEqual(options, { thread: true }); reads++; return new Promise(resolve => { release = resolve; }); } },
    api: { action() { assert.fail("retry cannot mutate a session"); } },
  });
  assert.equal(controller.canControl(), false, "even a stale ready snapshot cannot authorize a write");
  const retry = controller.retryConnection(); await controller.retryConnection();
  assert.equal(reads, 1); assert.equal(controller.checkingConnection, true);
  release(); await retry;
  assert.equal(controller.checkingConnection, false); assert.equal(paints, 1);
  assert.equal(node("composeInput").value, "Keep my unsent draft");
  assert.equal(attachments.items("a").length, 1);
});

test("writer conflict is shown once without raw-error or toast duplication", () => {
  nodes.clear(); const toasts = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", latestError: null, error: node("error"), paintBlockingState() {}, fitComposer() {}, paint() {},
    actions: { toast(message) { toasts.push(message); } },
  });
  controller.report({ code: "thread_in_use", message: "Original RPC diagnostic" });
  assert.equal(controller.error.hidden, true); assert.equal(controller.error.textContent, "");
  assert.deepEqual(toasts, []);
  controller.report({ code: "unavailable", message: "Connection lost" });
  assert.equal(controller.error.hidden, false); assert.deepEqual(toasts, ["Connection lost"]);
});

for (const malformed of [null, [], 17, "text", {}, { ok: false }, { ok: false, error: [] }, { ok: false, error: { code: 17, message: "bad" } }]) {
  test(`malformed action envelope ${JSON.stringify(malformed)} retains the same manual retry identity`, async () => {
    const ids = [], accepted = new Set();
    const api = new GatewayApi(async (_url, request) => {
      const id = JSON.parse(request.body).actionId; ids.push(id); accepted.add(id);
      return { ok: true, status: 200, json: async () => ids.length === 1 ? malformed : { ok: true } };
    });
    const controller = Object.assign(Object.create(MonitorControls.prototype), {
      selected: "a", pending: false, uncertainActions: new Map(), state: {}, api, paint() {}, recovery: { recover() {} },
    });
    const payload = { threadId: "a", text: "One accepted message", intent: "next" };
    await assert.rejects(controller.perform("send", payload), error => error instanceof GatewayError && error.code === "uncertain" && error.cause.message.includes("/api/codex/send"));
    assert.equal(ids.length, 1, "Invalid replies never replay an action automatically.");
    assert.equal(controller.pending, false); assert.equal(controller.uncertainActions.size, 1);
    await controller.perform("send", payload);
    assert.equal(new Set(ids).size, 1); assert.equal(accepted.size, 1); assert.equal(controller.uncertainActions.size, 0);
  });
}

for (const route of ["send", "create", "settings", "interrupt", "respond", "queue/cancel", "queue/steer", "archive"]) {
  test(`explicit ${route} retry after a lost accepted response reuses its identity without automatic replay`, async () => {
    const accepted = new Map(), ids = [];
    let drop = true, reads = 0;
    const api = new GatewayApi(async (_url, request) => {
      const body = JSON.parse(request.body); ids.push(body.actionId);
      accepted.set(body.actionId, body);
      if (drop) { drop = false; throw new Error("Response lost after native acceptance"); }
      return { ok: true, json: async () => ({ ok: true }) };
    });
    const controller = Object.assign(Object.create(MonitorControls.prototype), {
      selected: "thread-a", pending: false, uncertainActions: new Map(), state: {}, api, paint() {},
      recovery: { recover() { reads++; } },
    });
    const payload = { threadId: "thread-a", text: "One operation", intent: "next" };
    await assert.rejects(controller.perform(route, payload), error => error.code === "uncertain");
    assert.equal(ids.length, 1); assert.equal(reads, 1); assert.equal(controller.pending, false);
    await controller.perform(route, payload);
    assert.equal(ids[0], ids[1]); assert.equal(accepted.size, 1); assert.equal(controller.uncertainActions.size, 0);
    await controller.perform(route, payload); assert.notEqual(ids[1], ids[2]);
  });
}

test("different thread or content cannot consume another uncertain send identity; definitive failure retires it", async () => {
  const ids = [];
  let code = "uncertain";
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", pending: false, uncertainActions: new Map(), state: {}, paint() {}, recovery: { recover() {} },
    api: { action: async (_route, _payload, id) => { ids.push(id); throw Object.assign(new Error(code), { code, ...(code === "invalid_input" ? { receipt: { status: "rejected" } } : {}) }); } },
  });
  const one = { threadId: "a", text: "one" };
  await assert.rejects(controller.perform("send", one));
  await assert.rejects(controller.perform("send", { threadId: "b", text: "one" }));
  await assert.rejects(controller.perform("send", { threadId: "a", text: "two" }));
  assert.equal(new Set(ids).size, 3);
  code = "invalid_input";
  await assert.rejects(controller.perform("send", one)); assert.equal(ids[3], ids[0]);
  await assert.rejects(controller.perform("send", one)); assert.notEqual(ids[4], ids[0]);
});

test("parent to an unlisted child fences every native writer and restores only the parent's draft", async () => {
  nodes.clear();
  indexState.rows = new Map([["parent-rollout", { id: "parent-rollout", nativeThreadId: "parent", name: "Parent", cwd: "/repo" }]]);
  indexState.selected = "parent-rollout";
  let delivered = 0, forgotten = null;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "parent", generation: 1, state: { thread: { id: "parent", turns: [{ id: "turn", status: "inProgress" }] }, settings: { model: "m", effort: "low", cwd: "/repo" }, epoch: 1, connection: "connected", accessMode: "active", needsSnapshot: false },
    catalogEpoch: 1, models: [{ id: "m", model: "m", label: "Model", efforts: [{ value: "low" }], tiers: [{ id: "priority", name: "Fast" }] }],
    pending: false, creating: false, drafts: new Map(), attachments: new ImageAttachmentDrafts(), attachButton: node("attach"), paintImages() {}, error: node("error"), status: node("status"), pickButton: node("picker"), modelName: node("model-name"), modelEffort: node("model-effort"), fastButton: node("fast"), stopButton: node("stop"), steerButton: node("steer"), archiveSessionButton: node("archive"),
    recovery: { forget(id) { forgotten = id; }, select() {} }, actions: { refreshIndex() {}, toast() {} }, paintPending() {},
    perform: async () => { delivered++; return { ok: true }; },
  });
  node("composeInput").value = "Parent draft"; controller.paint(); assert.equal(node("sendBtn").disabled, false);
  indexState.selected = "unlisted-child"; indexState.selectedWasRow = false; controller.paint();
  assert.equal(forgotten, "parent"); assert.equal(controller.selected, null); assert.equal(controller.state.thread, null);
  for (const control of [node("sendBtn"), controller.pickButton, controller.fastButton, controller.stopButton, controller.steerButton, controller.archiveSessionButton]) assert.equal(control.disabled, true);
  await controller.send("next"); assert.equal(delivered, 0); assert.equal(node("composeInput").value, ""); assert.equal(controller.drafts.get("parent"), "Parent draft");
  indexState.selected = "parent-rollout"; controller.paint(); assert.equal(controller.selected, "parent"); assert.equal(node("composeInput").value, "Parent draft"); assert.equal(node("sendBtn").disabled, true);
  indexState.selected = ""; indexState.rows.clear();
});


test("a definitively unavailable deep link stays read-only without blocking other native history or reopening repeatedly", async () => {
  const originalFetch = globalThis.fetch;
  let opens = 0;
  const reports = [];
  globalThis.fetch = async () => ({ ok: true, json: async () => ({ groups: [] }) });
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    initialThread: "missing", archived: false, projectId: "", projects: [],
    api: { readMonitorIndex: async () => ({ groups: [] }), read: async () => ({ data: [{ id: "saved", cwd: "/repo", status: { type: "idle" } }], nextCursor: null }), open: async () => { opens++; throw new GatewayError("no rollout found", "rejected"); } },
    report(error) { reports.push(error.message); },
  });
  try {
    const groups = await controller.loadSessions();
    assert.equal(groups.groups[0].rows[0].nativeThreadId, "saved"); assert.equal(controller.initialThread, null);
    assert.equal(controller.sessionIdForThread("missing"), "native:missing");
    assert.equal(controller.sessionReadFailure("native:missing").message, "no rollout found");
    assert.equal(controller.sessionReadFailure("native:saved"), null);
    await controller.loadSessions(); assert.equal(opens, 1); assert.deepEqual(reports, ["no rollout found"]);
  } finally { globalThis.fetch = originalFetch; }
});

test("temporary deep-link transport failure retains the requested identity for recovery", async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => ({ ok: true, json: async () => ({ groups: [] }) });
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    initialThread: "requested", archived: false, projectId: "", projects: [],
    api: { readMonitorIndex: async () => ({ groups: [] }), read: async () => ({ data: [], nextCursor: null }), open: async () => { throw new GatewayError("Disconnected", "unavailable", null, undefined, true); } },
  });
  try { await assert.rejects(controller.loadSessions(), error => error.code === "unavailable"); assert.equal(controller.initialThread, "requested"); assert.equal(controller.unavailableThread, undefined); }
  finally { globalThis.fetch = originalFetch; }
});


test("image-only send captures native images and retires only accepted content even across selection changes", async () => {
  nodes.clear();
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  await attachments.add("a", [{ name: "one.png", type: "image/png", size: 8, url: "data:image/png;base64,first" }]);
  let release, delivered;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, state: {}, attachments, drafts: new Map(), paint() {}, report() {}, actions: { refreshIndex() {} },
    perform: async (_route, payload) => { delivered = payload; await new Promise(resolve => { release = resolve; }); },
  });
  const sending = controller.send("next");
  assert.deepEqual(delivered, { threadId: "a", text: "", images: ["data:image/png;base64,first"], intent: "next" });
  await attachments.add("a", [{ name: "two.png", type: "image/png", size: 8, url: "data:image/png;base64,second" }]);
  controller.selected = "b"; node("composeInput").value = "Other session draft";
  release(); await sending;
  assert.equal(node("composeInput").value, "Other session draft");
  assert.deepEqual(attachments.snapshot("a").map(image => image.url), ["data:image/png;base64,second"]);
});

test("image send failure retains draft and attachment; changed image cannot reuse uncertain action identity", async () => {
  nodes.clear(); node("composeInput").value = "Inspect";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  const file = url => ({ name: "one.png", type: "image/png", size: 8, url });
  await attachments.add("a", [file("data:image/png;base64,first")]);
  const ids = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, canControl: () => true, pending: false, state: {}, attachments, drafts: new Map(), uncertainActions: new Map(), paint() {}, report() {}, actions: { refreshIndex() {} }, recovery: { recover() {} },
    api: { action: async (_route, _payload, id) => { ids.push(id); throw new GatewayError("Lost response", "uncertain"); } },
  });
  await controller.send("next"); await controller.send("next");
  assert.equal(ids[0], ids[1]); assert.equal(node("composeInput").value, "Inspect"); assert.equal(attachments.snapshot("a").length, 1);
  attachments.remove("a", attachments.items("a")[0].id); await attachments.add("a", [file("data:image/png;base64,changed")]);
  await controller.send("next"); assert.notEqual(ids[2], ids[0]);
});


test("effort changes normalize native Standard tier and close only their original editor", async () => {
  let submitted, release, closes = 0;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", generation: 1, models: [{ id: "m", efforts: [{ value: "low" }] }], report() {},
    perform: async (_route, payload) => { submitted = payload; await new Promise(resolve => { release = resolve; }); }, closeSettings() { closes++; },
  });
  const operation = controller.performSettings({ model: "m", effort: "low", serviceTier: "default" });
  assert.equal(submitted.serviceTier, null);
  controller.selected = "b"; controller.generation++; release(); await operation; assert.equal(closes, 0);
});

test("a delayed image read error stays with its thread and is reported when that thread returns", async () => {
  nodes.clear();
  let rejectRead;
  const reports = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", state: { accessMode: "active", connection: "connected" }, generation: 1, drafts: new Map(), models: [],
    attachments: new ImageAttachmentDrafts(() => new Promise((_resolve, reject) => { rejectRead = reject; })),
    report(error) { if (error) reports.push(error.message); },
    recovery: { select() {} }, pending: false, creating: false, paintImages() {}, paintPending() {}, actions: {},
    status: node("status"), pickButton: node("picker"), modelName: node("model-name"), modelEffort: node("model-effort"), fastButton: node("fast"), attachButton: node("attach"), stopButton: node("stop"), steerButton: node("steer"), archiveSessionButton: node("archive"),
  });
  const reading = controller.addImages([{ name: "denied.png", type: "image/png", size: 8 }]);
  const nested = new Error("Fixture device I/O failure");
  const original = Object.assign(new Error("Fixture file read denied", { cause: nested }), { code: "EIO" });
  controller.selected = "b"; rejectRead(original); await reading;
  const saved = controller.imageErrors.get("a");
  assert.ok(saved instanceof AggregateError); assert.equal(saved.errors.length, 1);
  assert.equal(saved.errors[0].cause, original); assert.equal(saved.errors[0].cause.code, "EIO"); assert.equal(saved.errors[0].cause.cause, nested);
  assert.deepEqual(reports, []); assert.match(controller.imageErrors.get("a").message, /denied.png: Fixture file read denied/);
  indexState.rows = new Map([["a", { id: "a", nativeThreadId: "a", name: "A", cwd: "/repo" }]]); indexState.selected = "a";
  controller.paint(); assert.deepEqual(reports, ["denied.png: Fixture file read denied"]);
  indexState.selected = ""; indexState.rows.clear();
});

for (const successor of [false, true]) test(`uncertain image steering retains its original receipt after ${successor ? "a successor starts" : "its turn ends"}`, async () => {
  nodes.clear(); node("composeInput").value = "Inspect";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  await attachments.add("a", [{ name: "one.png", type: "image/png", size: 8, url: "data:image/png;base64,first" }]);
  const requests = [], receipts = new Set(); let drop = true, effects = 0;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, canControl: () => true, pending: false, state: { thread: { id: "a", turns: [{ id: "original", status: "inProgress" }] } },
    attachments, drafts: new Map(), uncertainActions: new Map(), paint() {}, report() {}, actions: { refreshIndex() {} }, recovery: { recover() {} },
    api: { action: async (_route, payload, id) => {
      requests.push({ id, payload }); if (!receipts.has(id)) { receipts.add(id); effects++; }
      if (drop) { drop = false; throw new GatewayError("Lost accepted response", "uncertain"); }
      return { ok: true };
    } },
  });
  await controller.send("steer"); assert.equal(attachments.snapshot("a").length, 1);
  controller.state.thread.turns[0].status = "completed";
  if (successor) controller.state.thread.turns.push({ id: "successor", status: "inProgress" });
  await controller.send(successor ? "steer" : "next");
  assert.equal(requests.length, 2); assert.equal(effects, 1); assert.equal(requests[0].id, requests[1].id);
  assert.deepEqual(requests[1].payload, requests[0].payload); assert.equal(requests[1].payload.expectedTurnId, "original");
  assert.equal(controller.uncertainSendPayloads.size, 0); assert.equal(attachments.snapshot("a").length, 0);
});

for (const laterImage of [false, true]) test(`accepted image send preserves ${laterImage ? "unchanged text with a later image" : "identical re-entered text"}`, async () => {
  nodes.clear(); node("composeInput").value = "Inspect";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  const file = url => ({ name: "one.png", type: "image/png", size: 8, url });
  await attachments.add("a", [file("data:image/png;base64,first")]);
  let release;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, state: {}, attachments, drafts: new Map([["a", "Inspect"]]), paint() {}, report() {}, actions: { refreshIndex() {} },
    perform: async () => new Promise(resolve => { release = resolve; }),
  });
  attachments.changed = thread => controller.touchDraft(thread);
  const sending = controller.send("next");
  if (laterImage) await attachments.add("a", [file("data:image/png;base64,second")]);
  else { node("composeInput").value = ""; controller.touchDraft("a"); node("composeInput").value = "Inspect"; controller.touchDraft("a"); }
  release(); await sending;
  assert.equal(node("composeInput").value, "Inspect"); assert.equal(controller.drafts.get("a"), "Inspect");
  assert.deepEqual(attachments.snapshot("a").map(image => image.url), laterImage ? ["data:image/png;base64,second"] : []);
});

test("accepted image send preserves a newer identical text draft after leaving its session", async () => {
  nodes.clear(); node("composeInput").value = "Inspect";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  await attachments.add("a", [{ name: "one.png", type: "image/png", size: 8, url: "data:image/png;base64,first" }]);
  let release;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, state: {}, attachments, drafts: new Map([["a", "Inspect"]]), paint() {}, report() {}, actions: { refreshIndex() {} },
    perform: async () => new Promise(resolve => { release = resolve; }),
  });
  const sending = controller.send("next");
  controller.touchDraft("a"); controller.touchDraft("a");
  controller.selected = "b"; node("composeInput").value = "Other session draft";
  release(); await sending;
  assert.equal(controller.drafts.get("a"), "Inspect"); assert.equal(node("composeInput").value, "Other session draft");
  assert.equal(attachments.snapshot("a").length, 0);
});


test("receipt recovery preserves identical reattached images and newer text", async () => {
  nodes.clear(); node("composeInput").value = "Inspect";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  const file = { name: "one.png", type: "image/png", size: 8, url: "data:image/png;base64,first" };
  await attachments.add("a", [file]);
  const original = attachments.snapshot("a")[0];
  let drop = true, effects = 0;
  const receipts = new Set(), requests = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, canControl: () => true, pending: false,
    state: { thread: { id: "a", turns: [{ id: "original", status: "inProgress" }] } },
    attachments, drafts: new Map([["a", "Inspect"]]), uncertainActions: new Map(),
    paint() {}, report() {}, actions: { refreshIndex() {} }, recovery: { recover() {} },
    api: { action: async (_route, payload, id) => {
      requests.push({ payload, id });
      if (!receipts.has(id)) { receipts.add(id); effects++; }
      if (drop) { drop = false; throw new GatewayError("Lost accepted response", "uncertain"); }
      return { ok: true };
    } },
  });
  attachments.changed = thread => controller.touchDraft(thread);
  await controller.send("steer");
  controller.state.thread.turns[0].status = "completed";
  attachments.remove("a", original.id); await attachments.add("a", [file]);
  const replacement = attachments.snapshot("a")[0]; assert.notEqual(replacement.id, original.id);
  await controller.send("next");
  assert.equal(effects, 1); assert.equal(requests[0].id, requests[1].id);
  assert.deepEqual(requests[0].payload, requests[1].payload);
  assert.deepEqual(attachments.snapshot("a"), [replacement]);
  assert.equal(node("composeInput").value, "Inspect"); assert.equal(controller.drafts.get("a"), "Inspect");
  assert.equal(controller.sendDrafts.size, 0);
});

test("queued message steering forwards its identity and current turn without consuming the draft", async () => {
  const actions = [], reads = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", state: { thread: { turns: [{ id: "active", status: "inProgress" }] }, queue: [{ clientUserMessageId: "queued", input: [{ type: "image", url: "original-image" }] }] },
    canControl: () => true, perform: async (...args) => actions.push(args), recovery: { recover: value => reads.push(value) },
  });
  node("composeInput").value = "Keep my unrelated draft";
  await controller.controlQueued("steer", "queued");
  assert.deepEqual(actions, [["queue/steer", { threadId: "a", clientUserMessageId: "queued", expectedTurnId: "active" }]]);
  assert.equal(node("composeInput").value, "Keep my unrelated draft");
  assert.deepEqual(reads, [{ thread: true }]);
  controller.state.thread.turns[0].status = "completed";
  await assert.rejects(controller.controlQueued("steer", "queued"), /turn has finished/);
  await controller.controlQueued("cancel", "queued");
  assert.deepEqual(actions[1], ["queue/cancel", { threadId: "a", clientUserMessageId: "queued" }]);
  controller.state.queue = [];
  await assert.rejects(controller.controlQueued("steer", "queued"), /already resolved/);
  controller.canControl = () => false;
  await assert.rejects(controller.controlQueued("steer", "queued"), /Reconnect/);
  assert.equal(actions.length, 2);
});


const createdEmptySnapshot = () => ({ epoch: 1, sequence: 2, thread: { id: "created", path: "/repo/rollout-created.jsonl", turns: [] }, settings: { cwd: "/repo" } });

test("a fast missing rollout uses the creation proof while native resume is pending, then retires it", async () => {
  const originalFetch = globalThis.fetch, calls = []; let release;
  const snapshot = createdEmptySnapshot();
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", generation: 1, observedEpoch: 1, createdSnapshot: snapshot,
    state: { connection: "connected", thread: null, epoch: 1, sequence: 0 }, paint() {},
    api: { open: () => new Promise(resolve => { release = resolve; }) },
  });
  const store = new RecordStore({ empty: () => calls.push("empty"), error: error => calls.push(error.message) }, { emptySession: id => controller.isEmptySession(id) });
  store.session = "rollout-created"; store.generation = 1;
  controller.actions = { refreshEmpty: () => store.refreshEmpty() };
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  const opening = controller.openThread("created");
  try {
    await store.poll(1); store.stop();
    assert.deepEqual(calls, ["empty"]);
    assert.equal(controller.state.thread, null, "The creation proof does not enable native writes before resume");
    release({ snapshot }); await opening;
    assert.equal(controller.createdSnapshot, null);
    controller.state.thread = { ...snapshot.thread, turns: [{ id: "first-turn", status: "inProgress" }] };
    assert.equal(controller.isEmptySession("rollout-created"), false);
  } finally { release({ snapshot }); await opening; store.stop(); globalThis.fetch = originalFetch; }
});

test("creation empty proof requires the exact thread, rollout, epoch and connected runtime", () => {
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", observedEpoch: 1, createdSnapshot: createdEmptySnapshot(), state: { connection: "connected", thread: null },
  });
  assert.equal(controller.isEmptySession("rollout-created"), true);
  assert.equal(controller.isEmptySession("another-rollout"), false);
  controller.selected = "another-thread"; assert.equal(controller.isEmptySession("rollout-created"), false);
  controller.selected = "created"; controller.observedEpoch = 2; assert.equal(controller.isEmptySession("rollout-created"), false);
  controller.observedEpoch = 1; controller.state.connection = "reconnecting"; assert.equal(controller.isEmptySession("rollout-created"), false);
  controller.state.connection = "connected"; controller.createdSnapshot.thread.turns = undefined; assert.equal(controller.isEmptySession("rollout-created"), false);
});

test("a rejected native resume retires the creation proof so a missing history remains an error", async () => {
  const originalFetch = globalThis.fetch, calls = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", generation: 1, observedEpoch: 1, createdSnapshot: createdEmptySnapshot(),
    state: { connection: "connected", thread: null }, api: { open: async () => { throw new GatewayError("Native thread missing", "rejected"); } },
  });
  const store = new RecordStore({ error: error => calls.push(error.message) }, { emptySession: id => controller.isEmptySession(id) });
  store.session = "rollout-created"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    await assert.rejects(controller.openThread("created"), /Native thread missing/);
    assert.equal(controller.createdSnapshot, null);
    await store.poll(1); store.stop(); assert.deepEqual(calls, ["pull HTTP 404"]);
  } finally { store.stop(); globalThis.fetch = originalFetch; }
});

test("an older resume cannot retire a different newly created session's empty proof", async () => {
  let release;
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", generation: 1, createdSnapshot: createdEmptySnapshot(),
    api: { open: () => new Promise(resolve => { release = resolve; }) },
  });
  const opening = controller.openThread("created");
  const newer = { ...createdEmptySnapshot(), thread: { id: "newer", turns: [] } };
  controller.selected = "newer"; controller.generation++; controller.createdSnapshot = newer;
  release({ snapshot: createdEmptySnapshot() }); await opening;
  assert.equal(controller.createdSnapshot, newer);
});


for (const envelope of [true, false]) test(`a newer native ${envelope ? "connection envelope" : "connection event"} fences creation proof before status returns`, async () => {
  const previous = { fetch: globalThis.fetch, EventSource: globalThis.EventSource, addEventListener: globalThis.addEventListener };
  const calls = [];
  globalThis.EventSource = class { close() {} };
  globalThis.addEventListener = () => {};
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", observedEpoch: 1, createdSnapshot: createdEmptySnapshot(),
    state: { connection: "connected", thread: null, epoch: null, sequence: 0 }, paint() {}, report(error) { throw error; },
    recovery: { recover() { return new Promise(() => {}); } },
  });
  const store = new RecordStore({ empty: () => calls.push("empty"), error: error => calls.push(error.message) }, { emptySession: id => controller.isEmptySession(id) });
  store.session = "rollout-created"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    controller.connectEvents();
    assert.equal(controller.isEmptySession("rollout-created"), true);
    const event = envelope ? { epoch: 2, connection: "connected" } : { epoch: 2, sequence: 1, method: "gateway/connection", params: { state: "connected" } };
    controller.events.onmessage({ data: JSON.stringify(event) });
    assert.equal(controller.observedEpoch, 1, "The status response is still pending");
    await store.poll(1); store.stop();
    assert.deepEqual(calls, ["pull HTTP 404"]);
    assert.equal(controller.canControl(), false);
  } finally { store.stop(); Object.assign(globalThis, previous); }
});

test("leaving and returning to the same new session does not let an old resume retire its proof", async () => {
  const originalFetch = globalThis.fetch, calls = []; let release;
  const snapshot = createdEmptySnapshot();
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "created", generation: 1, observedEpoch: 1, createdSnapshot: snapshot,
    state: { connection: "connected", thread: null, epoch: null, sequence: 0 },
    api: { open: () => new Promise(resolve => { release = resolve; }) }, paint() {}, actions: { refreshEmpty() {} },
  });
  const store = new RecordStore({ empty: () => calls.push("empty"), error: error => calls.push(error.message) }, { emptySession: id => controller.isEmptySession(id) });
  store.session = "rollout-created"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    const oldResume = controller.openThread("created");
    controller.selected = "another"; controller.generation++;
    controller.selected = "created"; controller.generation++;
    release({ snapshot }); await oldResume;
    await store.poll(1); store.stop(); assert.deepEqual(calls, ["empty"]);
    controller.api.open = async () => ({ snapshot }); await controller.openThread("created");
    assert.equal(controller.createdSnapshot, null);
    assert.equal(controller.state.thread.id, "created");
  } finally { store.stop(); globalThis.fetch = originalFetch; }
});


for (const envelope of [true, false]) test(`a late creation or deep-link proof cannot forget an earlier native ${envelope ? "envelope" : "event"} when selection resets`, async () => {
  const previous = { fetch: globalThis.fetch, EventSource: globalThis.EventSource, addEventListener: globalThis.addEventListener };
  nodes.clear();
  globalThis.EventSource = class { close() {} };
  globalThis.addEventListener = () => {};
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "other", generation: 1, observedEpoch: 1, state: { ...freshWorkspace(), connection: "connected", epoch: 1 },
    catalogEpoch: 1, models: [], pending: false, creating: false, drafts: new Map(), attachments: new ImageAttachmentDrafts(),
    status: node("status"), pickButton: node("picker"), modelName: node("model-name"), modelEffort: node("model-effort"),
    fastButton: node("fast"), attachButton: node("attach"), stopButton: node("stop"), steerButton: node("steer"), archiveSessionButton: node("archive"),
    report(error) { throw error; }, recovery: { recover() { return new Promise(() => {}); }, select() {} }, paintImages() {}, paintPending() {},
  });
  const calls = [], store = new RecordStore({ empty: () => calls.push("empty"), error: error => calls.push(error.message) }, { emptySession: id => controller.isEmptySession(id) });
  store.session = "rollout-created"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    indexState.rows = new Map([["other-rollout", { id: "other-rollout", nativeThreadId: "other", name: "Other", cwd: "/repo" }]]);
    indexState.selected = "other-rollout";
    controller.connectEvents();
    const event = envelope ? { epoch: 2, connection: "connected" } : { epoch: 2, sequence: 1, method: "gateway/connection", params: { state: "connected" } };
    controller.events.onmessage({ data: JSON.stringify(event) });
    // The accepted HTTP response arrives after SSE; both proof producers use this snapshot field.
    controller.createdSnapshot = createdEmptySnapshot();
    indexState.rows = new Map([["rollout-created", { id: "rollout-created", nativeThreadId: "created", name: "Created", cwd: "/repo" }]]);
    indexState.selected = "rollout-created";
    controller.paint();
    assert.equal(controller.state.epoch, null, "Actual thread selection resets its native state");
    assert.equal(controller.observedEpoch, 1, "Status still has the older native epoch");
    await store.poll(1); store.stop(); assert.deepEqual(calls, ["pull HTTP 404"]);
    assert.equal(controller.canControl(), false);
    // Conflicting status and SSE evidence stays unconfirmed until the status read catches up.
    controller.createdSnapshot = { ...createdEmptySnapshot(), epoch: 2 };
    assert.equal(controller.isEmptySession("rollout-created"), false);
    controller.observedEpoch = 2;
    assert.equal(controller.isEmptySession("rollout-created"), true);
    // A later status observation must also fence a proof when the stream has not caught up.
    controller.events.onmessage({ data: JSON.stringify({ epoch: 1, connection: "connected" }) });
    controller.createdSnapshot = createdEmptySnapshot();
    assert.equal(controller.isEmptySession("rollout-created"), false);
  } finally { store.stop(); indexState.rows.clear(); indexState.selected = ""; Object.assign(globalThis, previous); }
});


test("foreground recovery replaces a closed stream, fences controls and never replays an action", () => {
  const previous = globalThis.EventSource;
  const streams = [];
  globalThis.EventSource = class { constructor() { this.closed = false; streams.push(this); } close() { this.closed = true; } };
  const reads = [], controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", state: { thread: { id: "a" }, epoch: 1, connection: "connected", needsSnapshot: false }, catalogEpoch: 1,
    createdSnapshot: { thread: { id: "a" } }, paint() {}, report(error) { throw error; }, recovery: { recover(options) { reads.push(options); } },
  });
  try {
    controller.connectEvents(); const obsolete = streams[0];
    controller.suspendEvents(); assert.equal(obsolete.closed, true); assert.equal(controller.state.needsSnapshot, true); assert.equal(controller.canControl(), false);
    controller.resumeEvents(); assert.equal(streams.length, 2); assert.equal(controller.catalogEpoch, null); assert.equal(controller.createdSnapshot, null);
    assert.deepEqual(reads, [{ catalog: true, thread: true }]);
    obsolete.onmessage({ data: JSON.stringify({ epoch: 8, connection: "connected" }) });
    obsolete.onerror(); assert.equal(controller.connectionEpoch, undefined); assert.equal(reads.length, 1);
    streams[1].onmessage({ data: JSON.stringify({ epoch: 2, connection: "connected" }) });
    assert.equal(controller.connectionEpoch, 2); assert.equal(reads.length, 2);
    assert.equal(controller.canControl(), false, "A stream handshake is not a confirmed thread snapshot");
  } finally { controller.events?.close(); globalThis.EventSource = previous; }
});

test("Send and model blockers are visible while legitimate capability and archive guards remain", () => {
  nodes.clear();
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", pending: false, creating: false, state: { thread: null, connection: "reconnecting", epoch: null }, models: [],
    blockedStatus: node("blocked"), blockedMessage: node("blocked-message"), reconnectButton: node("reconnect"), pickButton: node("picker"),
  });
  controller.paintBlockingState(); assert.equal(controller.blockedStatus.hidden, false); assert.equal(controller.reconnectButton.hidden, false);
  assert.match(controller.blockedMessage.textContent, /Reconnecting/);
  controller.state = { thread: { id: "a" }, connection: "connected", epoch: 1, accessMode: "active", settings: { model: "retired", effort: "high" } };
  controller.catalogEpoch = 1; controller.models = [{ id: "m", model: "m", efforts: [{ value: "low" }] }];
  controller.paintBlockingState(); assert.match(controller.blockedMessage.textContent, /Saved model retired is unavailable/); assert.equal(controller.reconnectButton.hidden, true);
  assert.equal(controller.ready(), false); assert.equal(controller.canControl(), true, "A supported model may still be selected");
  controller.state.accessMode = "archived"; controller.paintBlockingState(); assert.match(controller.blockedMessage.textContent, /Restore/); assert.equal(controller.canControl(), false);
  controller.state.accessMode = "active"; controller.state.settings = { model: "m", effort: "low" }; controller.paintBlockingState();
  assert.equal(controller.blockedStatus.hidden, true); assert.equal(controller.ready(), true);
});

test("an accepted action with a stalled response body releases pending and retains draft/image and retry identity", async () => {
  nodes.clear(); node("composeInput").value = "Inspect the original image";
  const attachments = new ImageAttachmentDrafts(async file => file.url);
  await attachments.add("a", [{ name: "image.png", type: "image/png", size: 8, url: "data:image/png;base64,original" }]);
  let expire, first = true, decoding = false;
  const ids = [], executed = new Set();
  const api = new GatewayApi(async (_url, request) => {
    const payload = JSON.parse(request.body); ids.push(payload.actionId); executed.add(payload.actionId);
    return { ok: true, json() { decoding = true; if (first) { first = false; return new Promise(() => {}); } return Promise.resolve({ ok: true }); } };
  }, { schedule(operation) { expire = operation; return 1; }, cancel() {} });
  const failures = [], controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", ready: () => true, canControl: () => true, pending: false, state: {}, api, attachments, drafts: new Map(), uncertainActions: new Map(),
    paint() {}, report(error) { if (error) failures.push(error); }, actions: { refreshIndex() {} }, recovery: { recover() {} },
  });
  const sending = controller.send("next"); while (!decoding) await Promise.resolve(); assert.equal(controller.pending, true);
  expire(); await sending;
  assert.equal(controller.pending, false); assert.equal(failures[0].code, "uncertain"); assert.equal(ids.length, 1);
  assert.equal(node("composeInput").value, "Inspect the original image"); assert.equal(attachments.snapshot("a")[0].url, "data:image/png;base64,original");
  await controller.send("next"); assert.equal(ids.length, 2); assert.equal(ids[0], ids[1]); assert.equal(executed.size, 1);
  assert.equal(controller.pending, false); assert.equal(node("composeInput").value, ""); assert.equal(attachments.snapshot("a").length, 0);
});

for (const recoveryEvent of ["foreground", "stream-error"]) test(`recovery ${recoveryEvent} rejects its older snapshot before painting or retiring the new read`, async () => {
  const previous = globalThis.EventSource; globalThis.EventSource = class { close() {} };
  let release, opens = 0, recovered = 0;
  const held = new Promise(resolve => { release = resolve; });
  const snapshot = (sequence, model) => ({ thread: { id: "a", turns: [] }, settings: { model, effort: "low" }, epoch: 1, sequence, connection: "connected", accessMode: "active" });
  const paints = [], controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", generation: 1, state: snapshot(1, "obsolete"), catalogEpoch: 1, observedEpoch: 1, connectionEpoch: 1,
    paint() { paints.push([this.state.settings.model, this.canControl()]); }, report(error) { if (error) throw error; }, actions: { refreshEmpty() {} },
    api: { open: async () => { const attempt = ++opens; if (attempt === 1) { await held; return { snapshot: snapshot(1, "obsolete") }; } return { snapshot: snapshot(2, "current") }; } },
  });
  const { WorkspaceRecovery } = await import("../src/codex-workspace/api.js");
  const recovery = controller.recovery = new WorkspaceRecovery({
    loadStatus: async () => { controller.state.connection = "connected"; return { epoch: 1, connection: "connected" }; }, loadCatalog: async () => { controller.catalogEpoch = 1; },
    openThread: id => controller.openThread(id), onError(error) { throw error; }, onRecovered() { recovered++; },
  }, "a");
  recovery.catalogReady = true;
  try {
    controller.connectEvents(); const loading = recovery.recover(); while (!opens) await Promise.resolve();
    if (recoveryEvent === "foreground") { controller.suspendEvents(); controller.resumeEvents(); controller.events.onmessage({ data: JSON.stringify({ epoch: 1, sequence: 2, connection: "connected" }) }); }
    else controller.events.onerror();
    release(); await loading;
    assert.equal(opens, 2); assert.equal(recovered, 1); assert.equal(controller.state.settings.model, "current"); assert.equal(controller.state.sequence, 2);
    assert(!paints.some(([model, writable]) => model === "obsolete" && writable), "Old state must never transiently enable controls after reconnect");
  } finally { controller.events?.close(); globalThis.EventSource = previous; }
});

test("an authentication refusal cannot retire an earlier uncertain send receipt", async () => {
  let expire, stage = 0;
  const ids = [], api = new GatewayApi(async (_url, request) => {
    ids.push(JSON.parse(request.body).actionId);
    if (++stage === 1) return { ok: true, json: () => new Promise(() => {}) };
    if (stage === 2) return new Response("Authentication required", { status: 401 });
    return { ok: true, json: async () => ({ ok: true }) };
  }, { schedule(operation) { expire = operation; return 1; }, cancel() {} });
  const controller = Object.assign(Object.create(MonitorControls.prototype), { selected: "a", state: {}, api, uncertainActions: new Map(), paint() {}, recovery: { recover() {} } });
  const payload = { threadId: "a", text: "One original instruction", intent: "steer", expectedTurnId: "original" };
  const sending = controller.perform("send", payload); const failure = assert.rejects(sending, error => error.code === "uncertain"); expire(); await failure;
  await assert.rejects(controller.perform("send", payload), error => error.code === "pairing_required");
  assert.equal(controller.uncertainActions.size, 1); assert.equal(controller.uncertainSendPayloads.size, 1);
  await controller.perform("send", payload); assert.equal(ids.length, 3); assert.equal(new Set(ids).size, 1); assert.equal(controller.uncertainActions.size, 0);
});


test("a delayed accepted action cannot release the newer foreground snapshot fence", async () => {
  let release; const held = new Promise(resolve => { release = resolve; });
  const controller = Object.assign(Object.create(MonitorControls.prototype), { selected: "a", generation: 1, state: { thread: { id: "a", turns: [] }, epoch: 1, sequence: 1, connection: "connected" }, uncertainActions: new Map(), paint() {}, recovery: { recover() {} }, api: { action: () => held } });
  const action = controller.perform("settings", { threadId: "a", model: "m", effort: "low" });
  controller.generation++; controller.state.needsSnapshot = true;
  release({ snapshot: { thread: { id: "a", turns: [] }, settings: { model: "obsolete", effort: "low" }, epoch: 1, sequence: 2, connection: "connected" } }); await action;
  assert.equal(controller.pending, false); assert.equal(controller.state.needsSnapshot, true); assert.equal(controller.state.sequence, 1);
});

test("receipt lookup failure preserves the earlier uncertain action until a definite receipt resolves it", async () => {
  let stage = 0; const ids = [];
  const api = new GatewayApi(async (_url, request) => {
    ids.push(JSON.parse(request.body).actionId);
    if (++stage === 1) throw new Error("Reply lost after acceptance");
    if (stage === 2) return { ok: false, status: 500, json: async () => ({ ok: false, error: { code: "gateway_error", message: "read durable receipt: database locked" } }) };
    return { ok: false, status: 409, json: async () => ({ ok: false, error: { code: "rejected", message: "Original native rejection" }, receipt: { status: "rejected" } }) };
  });
  const controller = Object.assign(Object.create(MonitorControls.prototype), { selected: "a", state: {}, api, uncertainActions: new Map(), paint() {}, recovery: { recover() {} } });
  const payload = { threadId: "a", text: "One original request", intent: "next" };
  await assert.rejects(controller.perform("send", payload), error => error.code === "uncertain");
  await assert.rejects(controller.perform("send", payload), error => error.code === "gateway_error");
  assert.equal(controller.uncertainActions.size, 1);
  await assert.rejects(controller.perform("send", payload), error => error.receipt?.status === "rejected");
  assert.equal(new Set(ids).size, 1); assert.equal(controller.uncertainActions.size, 0); assert.equal(controller.uncertainSendPayloads.size, 0);
});


test("an index authentication refusal fences controls through the shared controller before generic scan handling", async () => {
  const error = { hidden: true, textContent: "" }, stream = { closed: false, close() { this.closed = true; } };
  const controller = Object.assign(Object.create(MonitorControls.prototype), { selected: "a", browserAccess: "paired", state: { thread: { id: "a", turns: [] }, connection: "connected", needsSnapshot: false }, api: new GatewayApi(async () => new Response("Authentication required", { status: 401 })), events: stream, error, fitComposer() {}, paint() {}, actions: { toast() {} } });
  assert.equal(controller.canControl(), true);
  await assert.rejects(controller.loadSessions(), failure => failure.code === "pairing_required");
  assert.equal(controller.browserAccess, "pairing-required"); assert.equal(controller.canControl(), false); assert.equal(stream.closed, true); assert.match(error.textContent, /Pair this browser/);
});

test("send preview is visible before HTTP settles and follows the receipt's exact action ID", async () => {
  const { OutgoingMessages } = await import("../src/codex-workspace/outgoing.js");
  let resolve, sentId;
  const outgoing = new OutgoingMessages();
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "a", generation: 1, pending: false, uncertainActions: new Map(), outgoing,
    actions: { recordSnapshot: () => ({ session: "rollout", records: [] }) },
    isEmptySession: () => true, paint() {},
    state: { epoch: 1, sequence: 1 },
    api: { action(_route, _payload, id) { sentId = id; return new Promise(r => { resolve = r; }); } },
  });
  const promise = controller.perform("send", { threadId: "a", text: "Immediate" });
  assert.equal(outgoing.list("a")[0].id, sentId);
  assert.equal(outgoing.list("a")[0].status, "sending");
  controller.selected = "b"; controller.generation++;
  resolve({ ok: true }); await promise;
  assert.deepEqual(outgoing.list("b"), []);
  assert.equal(outgoing.list("a")[0].status, "accepted");
});


test("native initialization and transport loss preserve the ordinary session index", async () => {
  let reads = 0, recovered = 0; const reports = [];
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    state: { connection: "connecting" }, initialThread: "requested", threads: [], projects: [],
    api: { readMonitorIndex: async () => ({ groups: [{ kind: "project", rows: [{ id: "ordinary", agent: "claude" }] }] }),
      read: async () => { reads++; throw new GatewayError("Reconnecting", "unavailable"); } },
    report: error => reports.push(error.message), recovery: { recover: () => { recovered++; } },
  });
  assert.equal((await controller.loadSessions()).groups[0].rows[0].id, "ordinary");
  assert.equal(reads, 0); assert.equal(controller.initialThread, "requested");
  controller.state.connection = "connected";
  assert.equal((await controller.loadSessions()).groups[0].rows[0].id, "ordinary");
  assert.equal(reads, 1); assert.equal(recovered, 1); assert.equal(controller.state.needsSnapshot, true);
  assert.deepEqual(reports, ["Reconnecting"]); assert.equal(controller.initialThread, "requested");
});

test("an expired server receipt remains uncertain and cannot authorize a new send", async () => {
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    api: { read: async () => ({ ok: true, result: { ok: false, error: { code: "receipt_expired", message: "Review history" }, receipt: { actionId: "saved", status: "expired" } } }) },
  });
  await assert.rejects(controller.readReceipt("saved"), error => error.code === "uncertain" && error.receipt.actionId === "saved");
});

test("browser storage refusal prevents the first HTTP send", async () => {
  let sent = 0;
  const failure = new Error("Storage quota exceeded");
  const controller = Object.assign(Object.create(MonitorControls.prototype), {
    selected: "t", generation: 0, pending: false, uncertainActions: new Map(), uncertainSendPayloads: new Map(),
    journal: { tab: "tab", prepare: async () => { throw failure; }, settle: async () => {} },
    persistDraft: async () => {}, draftRevision: () => 0, attachments: { snapshot: () => [] },
    actions: {}, paint: () => {}, api: { action: async () => { sent++; } },
  });
  await assert.rejects(controller.perform("send", { threadId: "t", text: "hello", intent: "next" }), failure);
  assert.equal(sent, 0); assert.equal(controller.pending, false);
});
