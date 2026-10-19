import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { modelOptions, selectedCapabilities, effectiveCapabilities, textOfInput, threadRecords, patchRows, freshWorkspace, applySnapshot, reduceEvent, activeTurn } from "../src/codex-workspace/model.js";
import { approvalDecisions, permissionResponse, questionResponse, requestResponsePayload, requestHtml, requestKind } from "../src/codex-workspace/requests.js";
import { GatewayApi, GatewayError, WorkspaceRecovery, applyWorkspaceEvent, preserveUncertainActionError, readAllPages } from "../src/codex-workspace/api.js";
import { Projection, viewRecord } from "../src/codex-ui/view-model.js";

const thread = () => ({ id: "thread-a", cwd: "/allowed/project", turns: [{ id: "turn-a", status: "inProgress", items: [{ id: "message-a", type: "agentMessage", text: "A", phase: "commentary" }] }] });
const snapshot = () => applySnapshot(freshWorkspace(), { thread: thread(), settings: { model: "m1", effort: "high" }, pending: [], queue: [], connection: "connected", epoch: 3, sequence: 10 });
const event = (method, params, extras = {}) => ({ method, params, epoch: 3, sequence: 11, ...extras });

test("malformed snapshot structures fail before replacing the current readable state", () => {
  const before = snapshot(), saved = structuredClone(before);
  const replacements = [
    { thread: { ...thread(), turns: [null] } },
    { thread: { ...thread(), turns: [{ id: "turn-a", items: [null] }] } },
    { thread: { ...thread(), turns: [{ id: "", items: [] }] } },
    { thread: { ...thread(), turns: [{ id: "turn-a", items: [{ type: "agentMessage" }] }] } },
    ...["pending", "queue"].flatMap(field => [null, {}, [null], [[]]].map(value => ({ thread: thread(), [field]: value }))),
    { thread: thread(), settings: [] },
  ];
  for (const value of replacements) {
    assert.throws(() => applySnapshot(before, value), /server returned (an )?invalid/i);
    assert.deepEqual(before, saved);
    assert.equal(activeTurn(before.thread).id, "turn-a"); assert.equal(threadRecords(before.thread).length, 1);
  }
  const future = thread(); future.turns[0].items.push({ id: "future-item", type: "futureOpaqueType", opaque: { value: 3 } });
  const after = applySnapshot(before, { thread: future });
  assert.deepEqual(after.thread.turns[0].items.at(-1), future.turns[0].items.at(-1));
  assert.doesNotThrow(() => threadRecords(after.thread));
});

for (const malformed of [null, [], 17, "text", {}, { ok: false }]) test(`malformed read envelope ${JSON.stringify(malformed)} is unavailable with operation context`, async () => {
  const api = new GatewayApi(async () => ({ ok: true, status: 200, json: async () => malformed }));
  await assert.rejects(api.open("a"), error => error instanceof GatewayError && error.code === "unavailable" && error.readOnly && error.cause.message.includes("/api/codex/open"));
  await assert.rejects(api.readMonitorIndex(), error => error.code === "unavailable" && error.readOnly && error.cause.message.includes("/api/sessions"));
});

const legacySnapshot = () => applySnapshot(freshWorkspace(), {
  thread: { ...thread(), historyMode: "legacy", turns: [{ id: "turn-a", status: "inProgress", items: [
    { id: "item-1", type: "userMessage", content: [{ type: "text", text: "Continue" }] },
    { id: "item-2", type: "agentMessage", text: "Same reply", phase: "commentary" },
  ] }] }, connection: "connected", epoch: 3, sequence: 10, pending: [{ id: 4, requestToken: "current-question" }],
});

test("legacy Core streams remain diagnostic when their identity overlaps saved history", () => {
  const before = legacySnapshot();
  const after = reduceEvent(before, event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "item-2", delta: "Same reply" }));
  assert.deepEqual(after.thread.turns[0].items, before.thread.turns[0].items);
  assert.equal(after.provisionalItems[0].item.id, "item-2");
  assert.equal(after.provisionalItems[0].item.partialFromJoin, true);
  const records = threadRecords(after.thread);
  assert.equal(records.filter(record => record.src === "Same reply").length, 1, "A saved alias and an unidentified Core stream cannot both represent transcript history.");
  assert.equal(records.at(-1).id, "item-2");
  assert.equal(records.some(record => record.id.startsWith("legacy-live:")), false);
});

test("legacy Core completion retires only provisional output while native persistence catches up", () => {
  const streamed = reduceEvent(legacySnapshot(), event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "item-2", delta: "unfinished" }));
  const completed = reduceEvent(streamed, event("gateway/legacy-history-changed", { threadId: "thread-a", turnId: "turn-a", itemId: "item-2", reason: "itemCompleted" }, { sequence: 12 }));
  assert.deepEqual(completed.provisionalItems, []);
  assert.deepEqual(completed.thread.turns[0].items, legacySnapshot().thread.turns[0].items);
  assert.equal(completed.legacyHistoryDirty, true); assert.equal(completed.needsSnapshot, false);
  assert.equal(completed.pending[0].requestToken, "current-question", "Display synchronization does not invalidate a native question.");
  const newer = reduceEvent(completed, event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "next-live-id", delta: "new suffix" }, { sequence: 13 }));
  const missing = applySnapshot(newer, { ...newer, thread: newer.thread, provisionalItems: newer.provisionalItems, sequence: 13 });
  assert.equal(missing.thread.turns[0].items.length, 2, "An early read does not invent the unavailable legacy record.");
  assert.equal(missing.provisionalItems[0].item.text, "new suffix");
  const materialized = structuredClone(missing.thread);
  materialized.turns[0].items.push({ id: "item-3", type: "agentMessage", text: "completed message" });
  const refreshed = applySnapshot(missing, { ...missing, thread: materialized, provisionalItems: missing.provisionalItems, sequence: 14 });
  assert.deepEqual(refreshed.thread.turns[0].items.map(item => item.id), ["item-1", "item-2", "item-3"]);
  assert.equal(refreshed.provisionalItems[0].item.id, "next-live-id");
});

test("legacy turn summary preserves all native full-read aliases and retires finished provisional items", () => {
  const streamed = reduceEvent(legacySnapshot(), event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-final", delta: "suffix" }));
  const after = reduceEvent(streamed, event("turn/completed", { threadId: "thread-a", turn: { id: "turn-a", status: "completed", completedAt: 123, items: [{ id: "live-final", type: "agentMessage", text: "summary only" }] } }, { sequence: 12 }));
  assert.deepEqual(after.thread.turns[0].items, legacySnapshot().thread.turns[0].items);
  assert.equal(after.thread.turns[0].status, "completed"); assert.equal(after.thread.turns[0].completedAt, 123);
  assert.deepEqual(after.provisionalItems, []); assert.equal(after.needsSnapshot, true);
});

test("legacy persisted identical-text messages retain both aliases across snapshots and rendering", () => {
  const value = legacySnapshot();
  value.thread.turns[0].items.push({ id: "item-3", type: "agentMessage", text: "Same reply" });
  const after = applySnapshot(value, { ...value, sequence: 20 });
  const replies = threadRecords(after.thread).filter(record => record.kind === "assistant");
  assert.deepEqual(replies.map(record => record.id), ["item-2", "item-3"]);
  assert.deepEqual(replies.map(record => record.src), ["Same reply", "Same reply"]);
});

test("legacy user and compaction events cannot append Core aliases into full-read history", () => {
  for (const type of ["userMessage", "contextCompaction"]) {
    for (const method of ["item/started", "item/completed"]) {
      const before = legacySnapshot();
      const after = reduceEvent(before, event(method, { threadId: "thread-a", turnId: "turn-a", item: { id: "core-only", type, content: [{ type: "text", text: "Continue" }] } }));
      assert.deepEqual(after.thread.turns[0].items, before.thread.turns[0].items);
      assert.deepEqual(after.provisionalItems, []); assert.equal(after.legacyHistoryDirty, true);
    }
  }
});

test("legacy partial reasoning retains only actual suffixes and bounded native indices", () => {
  const before = legacySnapshot();
  let after = reduceEvent(before, event("item/reasoning/summaryTextDelta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-thought", summaryIndex: 2, delta: "received reasoning" }));
  assert.deepEqual(after.provisionalItems[0].item.summary, ["", "", "received reasoning"]);
  assert.deepEqual(after.thread.turns[0].items, before.thread.turns[0].items);
  assert.equal(threadRecords(after.thread).some(record => record.src.includes("received reasoning")), false, "Unreconciled reasoning remains diagnostic until native saved history materializes.");
  after = reduceEvent(after, event("item/reasoning/textDelta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-thought", contentIndex: 1000000, delta: "invalid" }, { sequence: 12 }));
  assert.deepEqual(after.provisionalItems[0].item.content, []);
});

test("snapshot rejects unscoped, malformed, duplicate or archived provisional legacy output", () => {
  const before = legacySnapshot(), item = { id: "live-id", type: "agentMessage", text: "suffix" };
  const valid = { turnId: "turn-a", item };
  assert.equal(applySnapshot(before, { ...before, provisionalItems: [valid] }).provisionalItems[0].item.id, "live-id");
  for (const provisionalItems of [null, [null], [{ ...valid, turnId: "other-turn" }], [{ ...valid, item: { ...item, id: "" } }], [{ ...valid, item: { ...item, text: 5 } }], [{ ...valid, item: { ...item, type: "userMessage" } }], [valid, valid]]) {
    assert.throws(() => applySnapshot(before, { ...before, provisionalItems }), /invalid provisional legacy items/);
  }
  assert.throws(() => applySnapshot(before, { ...before, accessMode: "archived", provisionalItems: [valid] }), /invalid provisional legacy items/);
  assert.throws(() => applySnapshot(before, { ...before, thread: { ...before.thread, historyMode: "paginated" }, provisionalItems: [valid] }), /invalid provisional legacy items/);
  const completed = structuredClone(before.thread); completed.turns[0].status = "completed";
  assert.throws(() => applySnapshot(before, { ...before, thread: completed, provisionalItems: [valid] }), /invalid provisional legacy items/);
});

test("legacy event gaps, replaced connections and archive transitions discard only provisional output", () => {
  const streamed = reduceEvent(legacySnapshot(), event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-id", delta: "suffix" }));
  for (const notification of [event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-id", delta: "gap" }, { sequence: 13 }),
    event("gateway/connection", { connection: "reconnecting" }, { epoch: 4, sequence: 1 }), event("thread/archived", { threadId: "thread-a" }, { sequence: 12 })]) {
    const after = reduceEvent(streamed, notification);
    assert.deepEqual(after.provisionalItems, []); assert.deepEqual(after.thread.turns[0].items, streamed.thread.turns[0].items);
  }
  const unrelated = reduceEvent(streamed, event("gateway/legacy-history-changed", { threadId: "thread-b", turnId: "turn-a", itemId: "live-id", reason: "itemCompleted" }, { sequence: 12 }));
  assert.deepEqual(unrelated.provisionalItems, streamed.provisionalItems); assert.equal(unrelated.legacyHistoryDirty, false);
});

test("ordered notifications after archive cannot recreate legacy provisional output", () => {
  const before = reduceEvent(legacySnapshot(), event("thread/archived", { threadId: "thread-a" }));
  for (const notification of [event("item/started", { threadId: "thread-a", turnId: "turn-a", item: { id: "live-id", type: "agentMessage", text: "archived output" } }, { sequence: 12 }),
    event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-id", delta: "archived suffix" }, { sequence: 12 }),
    event("item/reasoning/textDelta", { threadId: "thread-a", turnId: "turn-a", itemId: "live-thought", delta: "archived reasoning" }, { sequence: 12 }),
    event("gateway/legacy-history-changed", { threadId: "thread-a", turnId: "turn-a", itemId: "live-id", reason: "itemCompleted" }, { sequence: 12 })]) {
    const after = reduceEvent(before, notification);
    assert.deepEqual(after.provisionalItems, []); assert.deepEqual(after.thread.turns[0].items, before.thread.turns[0].items);
    assert.equal(after.accessMode, "archived"); assert.equal(after.legacyHistoryDirty, false);
    assert.equal(after.sequence, 12, "An ignored private update still advances the shared sequence.");
  }
});

test("a legacy read racing terminal completion retains the Gateway's required follow-up read", () => {
  const before = legacySnapshot(), terminal = structuredClone(before.thread);
  terminal.turns[0].status = "completed";
  const raced = applySnapshot(before, { ...before, thread: terminal, legacyHistoryDirty: true, sequence: 15 });
  assert.equal(activeTurn(raced.thread), null);
  assert.equal(raced.legacyHistoryDirty, true, "Terminal metadata does not establish that an earlier history read included the final reply.");
  assert.equal(raced.needsSnapshot, false, "A display-only follow-up retains valid native control proofs.");
  const completed = structuredClone(terminal); completed.turns[0].items.push({ id: "item-3", type: "agentMessage", text: "Final reply" });
  const fresh = applySnapshot(raced, { ...raced, thread: completed, legacyHistoryDirty: false, sequence: 16 });
  assert.equal(fresh.legacyHistoryDirty, false); assert.equal(threadRecords(fresh.thread).at(-1).src, "Final reply");
  for (const legacyHistoryDirty of [null, "true", 1, {}]) assert.throws(() => applySnapshot(before, { ...before, legacyHistoryDirty }), /invalid legacy history synchronization state/);
  assert.throws(() => applySnapshot(before, { ...before, thread: { ...before.thread, historyMode: "paginated" }, legacyHistoryDirty: true }), /invalid legacy history synchronization state/);
  assert.throws(() => applySnapshot(before, { ...before, accessMode: "archived", legacyHistoryDirty: true }), /invalid legacy history synchronization state/);
});

test("live native plans survive snapshot and completion through the existing TodoWrite reader", () => {
  const update = { threadId: "thread-a", turnId: "turn-a", explanation: "PLAN_EXPLANATION <script>unsafe()</script>", plan: [{ step: "PLAN_COMPLETED", status: "completed" }, { step: "PLAN_RUNNING", status: "inProgress" }, { step: "PLAN_PENDING", status: "pending" }], futureMetadata: "PLAN_METADATA" };
  const next = reduceEvent(snapshot(), event("turn/plan/updated", update));
  assert.deepEqual(next.planUpdates, [update]);
  assert.equal(next.thread.turns[0].items.length, 1, "A notification never becomes a fabricated native ThreadItem.");
  const record = threadRecords(next.thread, undefined, next.planUpdates).at(-1), view = viewRecord(record);
  assert.equal(record.id, "turn-a:plan-update"); assert.equal(view.renderer, "tool"); assert.equal(view.name, "TodoWrite");
  for (const sentinel of ["Updated Plan", "PLAN_EXPLANATION", "Completed: PLAN_COMPLETED", "InProgress: PLAN_RUNNING", "Pending: PLAN_PENDING", "PLAN_METADATA"]) assert(view.html.includes(sentinel));
  assert(!view.html.includes("<script>")); assert.match(view.html, /&lt;script&gt;/);
  assert.deepEqual(record.nativeSource, update);
  const completed = { ...next.thread, turns: next.thread.turns.map(turn => ({ ...turn, status: "completed" })) };
  const refreshed = applySnapshot(next, { thread: completed, planUpdates: [update], epoch: 3, sequence: 12 });
  assert.deepEqual(refreshed.planUpdates, [update]); assert.match(viewRecord(threadRecords(refreshed.thread, undefined, refreshed.planUpdates).at(-1)).html, /PLAN_EXPLANATION/);
  const replacement = { ...update, explanation: null, plan: [] };
  assert.deepEqual(reduceEvent(next, event("turn/plan/updated", replacement, { sequence: 12 })).planUpdates, [replacement]);
});

test("early native plans retain one unknown turn until the native turn arrives", () => {
  const update = turnId => ({ threadId: "thread-a", turnId, explanation: null, plan: [{ step: turnId, status: "pending" }] });
  const first = reduceEvent(snapshot(), event("turn/plan/updated", update("future-turn")));
  assert.equal(first.needsSnapshot, true); assert.equal(first.thread.turns.length, 1);
  assert.equal(threadRecords(first.thread, undefined, first.planUpdates).length, 1);
  const second = reduceEvent(first, event("turn/plan/updated", update("replacement-turn"), { sequence: 12 }));
  assert.deepEqual(second.planUpdates, [update("replacement-turn")]);
  const known = reduceEvent(second, event("turn/started", { threadId: "thread-a", turn: { id: "replacement-turn", status: "inProgress", items: [] } }, { sequence: 13 }));
  assert.equal(known.needsSnapshot, true);
  assert.match(viewRecord(threadRecords(known.thread, undefined, known.planUpdates).at(-1)).html, /Pending: replacement-turn/);
  const intervening = applySnapshot(second, { thread: second.thread, epoch: 3, sequence: 12, planUpdates: [] });
  assert.equal(intervening.needsSnapshot, false);
  assert.equal(reduceEvent(intervening, event("turn/started", { threadId: "thread-a", turn: { id: "replacement-turn", status: "inProgress", items: [] } }, { sequence: 13 })).needsSnapshot, true, "The confirmed turn recovers an early Gateway plan omitted by the prior snapshot.");
  assert.deepEqual(reduceEvent(known, event("gateway/connection", { state: "connected" }, { epoch: 4, sequence: 1 })).planUpdates, []);
  assert.deepEqual(reduceEvent(known, event("gateway/recoveryError", { threadId: "thread-a", message: "scope closed" }, { sequence: 14 })).planUpdates, []);
});

test("native plan boundaries reject malformed snapshots and ignore malformed or foreign events", () => {
  const value = { threadId: "thread-a", turnId: "turn-a", plan: [{ step: "work", status: "pending" }] };
  for (const bad of [null, {}, { ...value, threadId: "other" }, { ...value, explanation: 4 }, { ...value, plan: [{ step: "work", status: "invented" }] }]) {
    assert.deepEqual(reduceEvent(snapshot(), event("turn/plan/updated", bad)).planUpdates, []);
    assert.throws(() => applySnapshot(snapshot(), { thread: thread(), planUpdates: [bad] }), /invalid native plan updates/);
  }
  assert.throws(() => applySnapshot(snapshot(), { thread: thread(), planUpdates: null }), /invalid native plan updates/);
  assert.deepEqual(applySnapshot(snapshot(), { thread: thread() }).planUpdates, []);
});

test("unknown user content retains its payload in transcript and queue text without active HTML", () => {
  const unknown = { type: "futureAttachment", label: "UNKNOWN_INPUT_SENTINEL", payload: { path: "/fixture/important-resource", markup: "<script>unsafe()</script>" } };
  const content = [{ type: "text", text: "Known prompt" }, { type: "localImage", path: "/fixture/image.png" }, { type: "skill", name: "fixture-skill" }, unknown, { note: "UNTYPED_INPUT_SENTINEL" }];
  const source = textOfInput(content);
  for (const sentinel of ["Known prompt", "[Image: /fixture/image.png]", "[Skill: fixture-skill]", "futureAttachment", "UNKNOWN_INPUT_SENTINEL", "/fixture/important-resource", "UNTYPED_INPUT_SENTINEL"]) assert(source.includes(sentinel), `Input retains ${sentinel}`);
  assert(source.includes(JSON.stringify(unknown, null, 2)));
  const records = threadRecords({ turns: [{ id: "t", status: "completed", items: [{ id: "u", type: "userMessage", content }] }] });
  assert.equal(records[0].src, source);
  const projection = new Projection(); projection.rebuild(records);
  assert.equal(projection.units[0].type, "user");
  const html = projection.units[0].view.html;
  assert.match(html, /UNKNOWN_INPUT_SENTINEL/); assert.match(html, /\/fixture\/important-resource/);
  assert.match(html, /&lt;script&gt;unsafe\(\)&lt;\/script&gt;/); assert(!html.includes("<script>"));
  assert.equal(textOfInput([null]), "null");
});

test("native exploration preserves semantic targets, compound output, and execution metadata", () => {
  const item = { id: "explore", type: "commandExecution", status: "completed", command: "cat src/lib.rs && rg needle src && rg --files", cwd: "/repo", aggregatedOutput: "EXPLORATION_OUTPUT\n", exitCode: 0, durationMs: 1230, commandActions: [
    { type: "read", command: "cat src/lib.rs", name: "lib.rs", path: "/repo/src/lib.rs" },
    { type: "search", command: "rg needle src", query: "needle", path: "/repo/src" },
    { type: "listFiles", command: "rg --files", path: "." },
  ] };
  const records = (actions, command = item.command) => threadRecords({ cwd: "/repo", turns: [{ id: "t", status: "completed", items: [{ ...item, command, commandActions: actions }] }] });
  const view = viewRecord(records(item.commandActions)[0]);
  assert.equal(view.renderer, "activity");
  assert.equal(view.summary, "Searched for 1 pattern, read 1 file, listed 1 directory");
  assert.deepEqual(view.children.map(child => [child.name, child.summary]), [["Read", "src/lib.rs"], ["Grep", "needle in src"], ["LS", "."]]);
  assert.equal(view.exit, 0); assert.equal(view.duration, "1.23s");
  assert.equal(view.children.at(-1).duration, "1.23s");
  assert.match(view.children.at(-1).html, /cat src\/lib.rs &amp;&amp; rg needle src &amp;&amp; rg --files/);
  assert.match(view.children.at(-1).html, /EXPLORATION_OUTPUT/);
  assert.match(view.children.at(-1).html, /Directory: \/repo/);
  const repeated = viewRecord(records([...item.commandActions, ...item.commandActions], `${item.command} && ${item.command}`)[0]);
  assert.equal(repeated.summary, "Searched for 2 patterns, read 1 file, listed 2 directories");
  assert.equal(repeated.children.length, 6, "Distinct-file summary retains every repeated native action.");
  for (const actions of [[], null, [...item.commandActions, { type: "unknown", command: "do-other-work" }], [...item.commandActions, null]]) {
    const negative = viewRecord(records(actions)[0]);
    assert.equal(negative.renderer, "bash"); assert.equal(negative.summary, item.command);
    assert.equal(negative.duration, "1.23s"); assert.match(negative.html, /EXPLORATION_OUTPUT/); assert.deepEqual(negative.children, []);
  }
});

test("native command duration preserves absent, zero, subsecond, second, and minute facts", () => {
  for (const [durationMs, expected] of [[null, ""], [undefined, ""], [0, "0ns"], [42, "42ms"], [1000, "1s"], [1230, "1.23s"], [65000, "1m 5s"], [-1, ""], [NaN, ""], ["42", ""]]) {
    const view = viewRecord(threadRecords({ turns: [{ id: "t", status: "completed", items: [{ id: "c", type: "commandExecution", status: "failed", command: "false", aggregatedOutput: null, exitCode: 7, durationMs }] }] })[0]);
    assert.equal(view.duration, expected); assert.equal(view.exit, 7); assert.equal(view.error, true);
    assert.match(view.pill, /failed/); assert.match(view.pill, /exit 7/);
  }
});

test("model switching uses advertised effort capabilities and defaults", () => {
  const models = modelOptions([{ id: "m1", isDefault: true, supportedReasoningEfforts: [{ reasoningEffort: "high" }, { reasoningEffort: "ultra" }], defaultReasoningEffort: "high" }, { id: "m2", supportedReasoningEfforts: ["low", "medium"], defaultReasoningEffort: "medium" }]);
  assert.deepEqual(selectedCapabilities(models, "m2", "ultra"), { model: "m2", effort: "medium", efforts: [{ value: "low", label: "low" }, { value: "medium", label: "medium" }] });
  assert.equal(selectedCapabilities(models, "unavailable", "ultra").model, "m1");
  assert.deepEqual(selectedCapabilities([], "m1", "high"), { model: "", effort: "", efforts: [] });
});

test("effective task settings preserve unavailable models and efforts without catalog defaults", () => {
  const models = modelOptions([{ id: "visible-id", model: "visible-model", isDefault: true, supportedReasoningEfforts: ["low", "high"], defaultReasoningEffort: "low" }]);
  for (const settings of [{ model: "absent-model", effort: "high" }, { model: "hidden-model", effort: "high" }, { model: "visible-model", effort: "obsolete-effort" }, { model: "absent-model", effort: "obsolete-effort" }]) {
    const actual = effectiveCapabilities(models, settings);
    assert.equal(actual.model, settings.model); assert.equal(actual.effort, settings.effort);
    assert.equal(actual.ready, false); assert.match(actual.reason, /unavailable/);
    assert.equal(selectedCapabilities(models, settings.model, settings.effort).model, "visible-id", "New-task and explicit choices still use advertised defaults.");
  }
});

test("effective task capabilities accept native catalog aliases and reasoningEffort spelling", () => {
  const models = modelOptions([{ id: "catalog-id", model: "canonical-model", supportedReasoningEfforts: ["low", "high"] }]);
  for (const model of ["catalog-id", "canonical-model"]) for (const settings of [{ model, effort: "high" }, { model, reasoningEffort: "high" }]) {
    const actual = effectiveCapabilities(models, settings);
    assert.equal(actual.model, model); assert.equal(actual.modelId, "catalog-id"); assert.equal(actual.effort, "high");
    assert.equal(actual.canonicalModel, "canonical-model");
    assert.equal(actual.ready, true); assert.equal(actual.reason, "");
  }
});

test("missing native settings and unavailable catalogs cannot enable messages", () => {
  const models = modelOptions([{ id: "one", supportedReasoningEfforts: ["low"] }]);
  for (const settings of [undefined, null, {}, { model: 4, effort: "low" }, { model: "one" }, { model: "one", effort: 4 }, { model: "", effort: "low" }, { model: "one", effort: "" }]) {
    const actual = effectiveCapabilities(models, settings);
    assert.equal(actual.ready, false); assert.match(actual.reason, /not confirmed/);
  }
  const unavailable = effectiveCapabilities([], { model: "one", effort: "low" });
  assert.equal(unavailable.model, "one"); assert.equal(unavailable.effort, "low"); assert.equal(unavailable.ready, false); assert.match(unavailable.reason, /catalog is unavailable/);
});

test("streaming native deltas preserve history and reject duplicates", () => {
  const before = snapshot();
  const delta = event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "message-a", delta: "B" });
  const after = reduceEvent(before, delta);
  assert.equal(after.thread.turns[0].items[0].text, "AB");
  assert.equal(before.thread.turns[0].items[0].text, "A");
  assert.equal(reduceEvent(after, delta), after);
});

test("late reader materializes only the received active-message suffix, then accepts canonical completion", () => {
  const state = snapshot(); state.thread.turns[0].items = [{ id: "user", type: "userMessage", content: [{ type: "text", text: "Late reader task" }] }];
  let next = reduceEvent(state, event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "already-started", delta: "received suffix" }));
  const item = next.thread.turns[0].items.at(-1);
  assert.equal(item.id, "already-started"); assert.equal(item.text, "received suffix"); assert.equal(item.partialFromJoin, true); assert.equal(item.phase, null);
  assert.equal(threadRecords(next.thread).at(-1).phase, "unknown");
  assert.match(threadRecords(next.thread).at(-1).body[1].x, /Joined while/);
  next = reduceEvent(next, event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "already-started", delta: " continues" }, { sequence: 12 }));
  assert.equal(next.thread.turns[0].items.at(-1).text, "received suffix continues");
  next = reduceEvent(next, event("item/completed", { threadId: "thread-a", turnId: "turn-a", item: { id: "already-started", type: "agentMessage", phase: "final_answer", text: "Canonical earlier prefix received suffix continues" } }, { sequence: 13 }));
  assert.equal(next.thread.turns[0].items.length, 2); assert.equal(next.thread.turns[0].items.at(-1).text, "Canonical earlier prefix received suffix continues"); assert.equal(next.thread.turns[0].items.at(-1).partialFromJoin, undefined);
  assert.equal(threadRecords(next.thread).at(-1).body.length, 1);
  assert.equal(state.thread.turns[0].items.length, 1);
});

test("orphan deltas for completed or unknown turns do not invent transcript items", () => {
  const state = snapshot(); state.thread.turns[0].status = "completed"; state.thread.turns[0].items = [];
  const after = reduceEvent(state, event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "late", delta: "stale" }));
  assert.deepEqual(after.thread.turns[0].items, []);
  const unknown = reduceEvent(snapshot(), event("item/agentMessage/delta", { threadId: "thread-a", turnId: "unknown-turn", itemId: "late", delta: "foreign" }));
  assert.equal(unknown.thread.turns.length, 1); assert.equal(unknown.thread.turns[0].items.length, 1);
});

test("global sequence advances for other threads without mixing their transcript", () => {
  const after = reduceEvent(snapshot(), event("item/agentMessage/delta", { threadId: "thread-b", turnId: "turn-a", itemId: "message-a", delta: "other" }));
  assert.equal(after.sequence, 11); assert.equal(after.thread.turns[0].items[0].text, "A");
});

test("missing event or new runtime epoch requires an authoritative snapshot", () => {
  for (const change of [{ sequence: 13 }, { epoch: 4 }]) {
    const after = reduceEvent(snapshot(), event("item/agentMessage/delta", { threadId: "thread-a", turnId: "turn-a", itemId: "message-a", delta: "missing" }, change));
    assert.equal(after.needsSnapshot, true); assert.equal(after.thread.turns[0].items[0].text, "A");
  }
});

test("native reconnect advances epoch and makes snapshot recovery possible", () => {
  let state = reduceEvent(snapshot(), event("gateway/connection", { connection: "reconnecting" }));
  assert.equal(state.connection, "reconnecting");
  state = reduceEvent(state, event("gateway/connection", { connection: "connected" }, { epoch: 4, sequence: 12 }));
  assert.equal(state.connection, "connected"); assert.equal(state.needsSnapshot, true);
});

test("Gateway server request IDs retain their native number/string identity", () => {
  let state = reduceEvent(snapshot(), event("item/tool/requestUserInput", { threadId: "thread-a", questions: [] }, { requestId: 17 }));
  assert.equal(state.pending[0].id, 17); assert.equal(state.pending[0].method, "item/tool/requestUserInput");
  state = reduceEvent(state, event("serverRequest/resolved", { threadId: "thread-a", requestId: "17" }, { sequence: 12 }));
  assert.equal(state.pending.length, 1);
  state = reduceEvent(state, event("serverRequest/resolved", { threadId: "thread-a", requestId: 17 }, { sequence: 13 }));
  assert.equal(state.pending.length, 0);
});

test("approval answers retain the request generation and selected native thread", () => {
  for (const id of [7, "7"]) {
    const request = { id, requestToken: "gateway:4:27", method: "item/commandExecution/requestApproval", params: { threadId: "thread-a", command: "current command" } };
    assert.deepEqual(requestResponsePayload(request, { decision: "accept" }), { threadId: "thread-a", requestId: id, requestToken: "gateway:4:27", response: { decision: "accept" } });
    assert.match(requestHtml(request, 0), /data-request-token="gateway:4:27"/);
    assert.throws(() => requestResponsePayload({ ...request, requestToken: undefined }, {}), /fresh task snapshot/);
  }
  const before = snapshot(); before.pending = [{ id: 7, requestToken: "old-gateway:3:9" }];
  for (const change of [{ epoch: 4 }, { sequence: 15 }]) {
    const after = reduceEvent(before, event("item/agentMessage/delta", { threadId: "thread-a" }, change));
    assert.equal(after.needsSnapshot, true); assert.deepEqual(after.pending, []);
  }
  const disconnected = reduceEvent(before, event("gateway/connection", { connection: "reconnecting" }));
  assert.deepEqual(disconnected.pending, []); assert.equal(disconnected.needsSnapshot, true);
  const replacement = { id: 7, requestToken: "new-request:3:11", method: "item/commandExecution/requestApproval", params: { threadId: "thread-a" } };
  assert.deepEqual(reduceEvent(before, event(replacement.method, replacement.params, { requestId: 7, request: replacement })).pending, [replacement]);
});

test("native archived rollout provenance belongs only to its read-only snapshot", () => {
  const recovered = applySnapshot(snapshot(), { thread: thread(), accessMode: "archived", historySource: { kind: "nativeArchivedRollout", historyMode: "paginated", bytes: 500 } });
  assert.equal(recovered.historySource.kind, "nativeArchivedRollout");
  assert.equal(applySnapshot(recovered, { thread: thread(), accessMode: "active" }).historySource, null);
  assert.equal(applySnapshot(recovered, { thread: thread(), accessMode: "archived" }).historySource, null);
  assert.throws(() => applySnapshot(recovered, { thread: thread(), accessMode: "active", historySource: recovered.historySource }), /invalid archived history provenance/);
  assert.throws(() => applySnapshot(recovered, { thread: thread(), accessMode: "archived", historySource: { ...recovered.historySource, bytes: "500" } }), /invalid archived history provenance/);
  const rawItem = { id: "original-id", type: "archivedCoreItem", sourceItem: { type: "futureNativeType", payload: "COLD_ARCHIVE_CONTENT <script>unsafe()</script>" } };
  const record = threadRecords({ turns: [{ id: "original-turn", status: "completed", items: [rawItem] }] })[0];
  assert.match(record.body[0].x, /original-id/); assert.match(record.body[0].x, /futureNativeType/);
  const view = viewRecord(record);
  assert.match(view.html, /COLD_ARCHIVE_CONTENT/); assert.match(view.html, /&lt;script&gt;/); assert(!view.html.includes("<script>"));
});

test("desktop dynamic tool completes via item/completed without browser execution", () => {
  let state = reduceEvent(snapshot(), event("item/tool/call", { threadId: "thread-a", callId: "tool-a", tool: "desktop-tool" }, { requestId: "native-1" }));
  assert.equal(requestKind(state.pending[0]), "desktop");
  state = reduceEvent(state, event("item/completed", { threadId: "thread-a", turnId: "turn-a", item: { id: "tool-a", type: "dynamicToolCall", status: "completed" } }, { sequence: 12 }));
  assert.equal(state.pending.length, 0);
});

test("turn completion keeps streamed items when native notification has an empty list", () => {
  const state = reduceEvent(snapshot(), event("turn/completed", { threadId: "thread-a", turn: { id: "turn-a", status: "interrupted", items: [] } }));
  assert.equal(state.thread.turns[0].items[0].text, "A"); assert.equal(activeTurn(state.thread), null); assert.equal(state.needsSnapshot, true);
});

test("shared settings notification updates the selected task capabilities", () => {
  const after = reduceEvent(snapshot(), event("thread/settings/updated", { threadId: "thread-a", threadSettings: { model: "m2", effort: "medium" } }));
  assert.equal(after.settings.model, "m2"); assert.equal(after.settings.effort, "medium");
});

test("an idle shared task immediately displays its updated execution directory", () => {
  const before = snapshot(); before.thread.turns[0].status = "completed";
  const after = reduceEvent(before, event("thread/settings/updated", { threadId: "thread-a", threadSettings: { model: "m2", effort: "medium", cwd: "/allowed/new-project" } }));
  assert.equal(activeTurn(after.thread), null);
  assert.equal(after.thread.cwd, "/allowed/new-project");
  assert.equal(after.settings.cwd, after.thread.cwd);
  assert.equal(before.thread.cwd, "/allowed/project");
  const unchanged = reduceEvent(before, event("thread/settings/updated", { threadId: "other-task", threadSettings: { cwd: "/allowed/other" } }));
  assert.equal(unchanged.thread.cwd, before.thread.cwd);
  const omitted = reduceEvent(before, event("thread/settings/updated", { threadId: "thread-a", threadSettings: { model: "m2" } }));
  assert.equal(omitted.thread.cwd, before.thread.cwd);
});

test("queue notification requires runtime refresh; it does not guess a submission state", () => {
  const after = reduceEvent(snapshot(), event("thread/queue/changed", { threadId: "thread-a", queuedSubmissionId: "q" }));
  assert.equal(after.needsSnapshot, true); assert.deepEqual(after.queue, []);
});

test("live file changes replace their patch before approval or completion", () => {
  const state = snapshot(); state.thread.turns[0].items.push({ type: "fileChange", id: "edit-a", changes: [] });
  const changes = [{ path: "/allowed/project/file.txt", diff: "+updated" }];
  const after = reduceEvent(state, event("item/fileChange/patchUpdated", { threadId: "thread-a", turnId: "turn-a", itemId: "edit-a", changes }));
  assert.deepEqual(after.thread.turns[0].items.at(-1).changes, changes); assert.deepEqual(state.thread.turns[0].items.at(-1).changes, []);
});

test("native file changes retain empty-file operations, rename destinations and supported legacy output", () => {
  const native = { turns: [{ id: "t", status: "completed", items: [{ id: "edit", type: "fileChange", status: "completed", aggregatedOutput: "LEGACY_PATCH_OUTPUT <script>unsafe()</script>", changes: [
    { path: "/repo/empty-added", kind: { type: "add" }, diff: "" },
    { path: "/repo/empty-deleted", kind: { type: "delete" }, diff: "" },
    { path: "/repo/original", kind: { type: "update", move_path: "/repo/RENAMED_DESTINATION" }, diff: "" },
    { path: "/repo/future", kind: { type: "futureChange", reason: "UNKNOWN_CHANGE_KIND" }, diff: "" },
  ] }] }] };
  const record = threadRecords(native)[0], view = viewRecord(record);
  assert.match(record.head.target, /\/repo\/original \(moved to \/repo\/RENAMED_DESTINATION\)/);
  for (const sentinel of ["Added file", "Deleted file", "Moved to: /repo/RENAMED_DESTINATION", "UNKNOWN_CHANGE_KIND", "LEGACY_PATCH_OUTPUT"]) assert(view.html.includes(sentinel), `Shared file-change view retains ${sentinel}`);
  assert.match(view.html, /&lt;script&gt;unsafe\(\)&lt;\/script&gt;/); assert(!view.html.includes("<script>"));
});

test("supported legacy fileChange output deltas reach the same shared presenter", () => {
  const before = snapshot(); before.thread.turns[0].items.push({ id: "patch", type: "fileChange", status: "inProgress", changes: [] });
  const params = { threadId: "thread-a", turnId: "turn-a", itemId: "patch", delta: "LIVE_PATCH_OUTPUT" };
  const after = reduceEvent(before, event("item/fileChange/outputDelta", params));
  assert.equal(after.thread.turns[0].items.at(-1).aggregatedOutput, params.delta);
  assert.match(viewRecord(threadRecords(after.thread).at(-1)).html, /LIVE_PATCH_OUTPUT/);
  assert.equal(before.thread.turns[0].items.at(-1).aggregatedOutput, undefined);
});

test("standalone native web search results retain opaque title, URL and snippet content safely", () => {
  const item = { id: "web", type: "webSearch", query: "Original query", action: { type: "search", query: "Original query" }, results: { pages: [{ title: "SEARCH_RESULT_TITLE", url: "https://fixture.invalid/SEARCH_RESULT_URL", snippet: "SEARCH_RESULT_SNIPPET <img src=x onerror=unsafe()>" }] } };
  const record = threadRecords({ turns: [{ id: "t", status: "completed", items: [item] }] })[0];
  assert.equal(record.head.target, "Original query");
  assert.equal(record.body.at(-1).x, JSON.stringify(item.results, null, 2));
  const view = viewRecord(record);
  for (const sentinel of ["SEARCH_RESULT_TITLE", "SEARCH_RESULT_URL", "SEARCH_RESULT_SNIPPET"]) assert(view.html.includes(sentinel));
  assert(!view.html.includes("<img")); assert.match(view.html, /&lt;img/);
});

test("snapshot validates its thread and restores pending requests and queue", () => {
  assert.throws(() => applySnapshot(freshWorkspace(), {}), /invalid thread snapshot/);
  const restored = applySnapshot(snapshot(), { thread: thread(), pending: [{ id: 2 }], queue: [{ id: "q" }], epoch: 4, sequence: 1 });
  assert.equal(restored.needsSnapshot, false); assert.equal(restored.pending[0].id, 2); assert.equal(restored.queue[0].id, "q");
  const recovered = applySnapshot({ ...snapshot(), lastError: "Old runtime failure" }, { thread: thread(), epoch: 4, sequence: 2 });
  assert.equal(recovered.lastError, null);
  assert.equal(reduceEvent(recovered, event("gateway/historyChanged", {}, { epoch: 4, sequence: 3 })).lastError, null);
});

test("archived snapshots expose history with unknown execution settings and no writer controls", () => {
  const archived = applySnapshot(snapshot(), { thread: thread(), accessMode: "archived", settings: { model: "stale-model" }, pending: [{ id: 2 }], queue: [{ id: "q" }], epoch: 3, sequence: 12 });
  assert.equal(archived.accessMode, "archived"); assert.equal(archived.thread.id, "thread-a");
  assert.deepEqual(archived.settings, {}); assert.deepEqual(archived.pending, []); assert.deepEqual(archived.queue, []);
  for (const invalid of ["readOnly", false, {}, 1, null]) assert.throws(() => applySnapshot(archived, { thread: thread(), accessMode: invalid }), /invalid thread access mode/);
  assert.equal(applySnapshot(archived, { thread: thread() }).accessMode, "active", "A fresh compatible snapshot clears the previous task's archive marker.");
  assert.equal(applySnapshot(archived, { thread: thread(), accessMode: "active" }).accessMode, "active");
});

test("ordered archive events fence writers until a successful post-restore snapshot", () => {
  const before = snapshot(); before.pending = [{ id: 1 }]; before.queue = [{ id: "q" }];
  const notification = event("thread/archived", { threadId: "thread-a" });
  const archived = reduceEvent(before, notification);
  assert.equal(archived.accessMode, "archived"); assert.deepEqual(archived.settings, {});
  assert.deepEqual(archived.pending, []); assert.deepEqual(archived.queue, []);
  assert.equal(reduceEvent(archived, notification), archived, "Duplicate events cannot change the access fence.");
  assert.equal(reduceEvent(before, event("thread/archived", { threadId: "thread-b" })).accessMode, "active");
  const restored = reduceEvent(archived, event("thread/unarchived", { threadId: "thread-a" }, { sequence: 12 }));
  assert.equal(restored.accessMode, "archived"); assert.equal(restored.needsSnapshot, true);
  assert.equal(reduceEvent(archived, event("thread/unarchived", { threadId: "thread-b" }, { sequence: 12 })).needsSnapshot, false);
  const reopened = applySnapshot(restored, { thread: thread(), accessMode: "active", settings: { model: "native-resumed-model" }, epoch: 3, sequence: 13 });
  assert.equal(reopened.accessMode, "active"); assert.equal(reopened.settings.model, "native-resumed-model"); assert.equal(reopened.needsSnapshot, false);
  const lateSettings = reduceEvent(archived, event("thread/settings/updated", { threadId: "thread-a", threadSettings: { model: "stale-model" } }, { sequence: 12 }));
  assert.deepEqual(lateSettings.settings, {}, "An archived reader cannot claim current execution settings from a late notification.");
  const replayedApproval = reduceEvent(archived, event("item/commandExecution/requestApproval", { threadId: "thread-a" }, { sequence: 12, requestId: "old-request" }));
  assert.deepEqual(replayedApproval.pending, [], "An archived reader cannot expose write controls from replayed approvals.");
});

test("native archive discovered during reconnect fences controls before the historical refresh", () => {
  const before = snapshot(); before.pending = [{ id: 1 }]; before.queue = [{ id: "q" }];
  const archived = reduceEvent(before, event("gateway/snapshot-required", { threadId: "thread-a", accessMode: "archived" }));
  assert.equal(archived.accessMode, "archived"); assert.equal(archived.needsSnapshot, true);
  assert.deepEqual(archived.pending, []); assert.deepEqual(archived.queue, []); assert.deepEqual(archived.settings, {});
  assert.equal(archived.thread.id, "thread-a");
  const foreign = reduceEvent(before, event("gateway/snapshot-required", { threadId: "thread-b", accessMode: "archived" }));
  assert.equal(foreign.accessMode, "active"); assert.equal(foreign.needsSnapshot, false);
  const premature = reduceEvent(archived, event("gateway/snapshot-required", { threadId: "thread-a", accessMode: "active" }, { sequence: 12 }));
  assert.equal(premature.accessMode, "archived", "Only an authoritative post-restore snapshot enables writers.");
});

test("recovery binds default browser timer receivers when retrying and cancelling", async () => {
  const originalSchedule = globalThis.setTimeout, originalCancel = globalThis.clearTimeout;
  const scheduled = [], cancelled = [];
  globalThis.setTimeout = function (callback, delay) {
    assert(this === globalThis, "Native browser setTimeout requires its global receiver.");
    scheduled.push({ callback, delay }); return 81;
  };
  globalThis.clearTimeout = function (id) {
    assert(this === globalThis, "Native browser clearTimeout requires its global receiver.");
    cancelled.push(id);
  };
  try {
    const recovery = new WorkspaceRecovery({ loadStatus: async () => ({ connection: "connected", epoch: 1 }), loadCatalog: async () => {}, openThread: async () => {}, onError: () => {}, onRecovered: () => {} });
    recovery.defer(); assert.equal(recovery.retry, 81); assert.equal(scheduled[0].delay, 500);
    await recovery.recover(); assert.deepEqual(cancelled, [81]); assert.equal(recovery.retry, null);
  } finally { globalThis.setTimeout = originalSchedule; globalThis.clearTimeout = originalCancel; }
});

test("normalized Codex records use the existing Monitor Projection for messages and process", () => {
  const native = thread();
  native.turns[0].items.unshift({ id: "user", type: "userMessage", content: [{ type: "text", text: "hello <script>" }] });
  native.turns[0].items.push({ id: "command", type: "commandExecution", command: "pwd", aggregatedOutput: "/project", exitCode: 0, status: "completed" }, { id: "final", type: "agentMessage", phase: "final_answer", text: "Done" });
  const records = threadRecords(native); const projection = new Projection(); projection.rebuild(records);
  assert.deepEqual(projection.units.map(unit => unit.type), ["user", "process", "assistant"]);
  assert.match(records[0].body[0].h, /&lt;script&gt;/); assert.equal(records.at(-1).phase, "final");
});

test("native item liveness is explicit rather than inferred from historical tool labels", () => {
  const value = thread();
  value.turns[0].items.push({ id: "active-command", type: "commandExecution", status: "inProgress", command: "sleep 1" });
  const records = threadRecords(value); assert.equal(records.at(-1).runtime.running, true);
  value.turns[0].items.at(-1).status = "completed";
  assert.equal(threadRecords(value).at(-1).runtime.running, false);
});

test("native patches retain file and hunk headers, signs and source lines", () => {
  const rows = patchRows("--- a/file.txt\n+++ b/file.txt\n@@ -3,2 +3,2 @@\n-old\n+++ actual added code\n same\n\\ No newline at end of file");
  assert.deepEqual(rows, [["ctx", null, "--- a/file.txt"], ["ctx", null, "+++ b/file.txt"], ["ctx", null, "@@ -3,2 +3,2 @@"], ["del", 3, "old"], ["add", 3, "++ actual added code"], ["ctx", 4, "same"], ["ctx", null, "\\ No newline at end of file"]]);
  assert.deepEqual(patchRows("-simple\n+change"), [["del", null, "simple"], ["add", null, "change"]]);
});

test("approval UI omits persistent grants and unsupported native choices", () => {
  assert.deepEqual(approvalDecisions({ params: { availableDecisions: ["acceptForSession", "decline", { acceptWithExecpolicyAmendment: {} }] } }), ["decline"]);
  const html = requestHtml({ id: 1, method: "item/commandExecution/requestApproval", params: { command: "echo <unsafe>", availableDecisions: ["accept", "decline"] } }, 0);
  assert.match(html, /approve-request/); assert.match(html, /echo &lt;unsafe&gt;/); assert.doesNotMatch(html, /acceptForSession/);
});

test("question response validates every answer and keeps native answer shape", () => {
  const request = { params: { questions: [{ id: "q1", header: "Choice" }, { id: "q2", question: "Name?" }] } };
  assert.throws(() => questionResponse(request, { q1: "A" }), /Name/);
  assert.deepEqual(questionResponse(request, { q1: " A ", q2: ["B"] }), { answers: { q1: { answers: ["A"] }, q2: { answers: ["B"] } } });
});

test("permission decisions are limited to the current turn and preserve requested permissions", () => {
  const request = { params: { permissions: { fileSystem: { write: ["/allowed"] } } } };
  assert.deepEqual(permissionResponse(request, false), { permissions: {}, scope: "turn" });
  assert.deepEqual(permissionResponse(request, true), { permissions: request.params.permissions, scope: "turn" });
});

test("HTTP actions use same-origin pairing, no cache, and a stable explicit receipt ID", async () => {
  const calls = []; const api = new GatewayApi(async (url, options) => { calls.push({ url, options }); return { ok: true, json: async () => ({ ok: true }) }; });
  await api.action("send", { threadId: "t", text: "hello", intent: "next" }, "submission-1");
  assert.equal(calls[0].options.credentials, "same-origin"); assert.equal(calls[0].options.cache, "no-store");
  assert.equal(JSON.parse(calls[0].options.body).actionId, "submission-1");
});

test("default browser fetch retains the global receiver required by Window.fetch", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async function () { assert.equal(this, globalThis); return { ok: true, json: async () => ({ ok: true, connection: "connected" }) }; };
  try { assert.equal((await new GatewayApi().read("status")).connection, "connected"); }
  finally { globalThis.fetch = original; }
});

test("uncertain writes never retry automatically and preserve Gateway receipt errors", async () => {
  let count = 0; const api = new GatewayApi(async () => { count++; throw new Error("connection lost"); });
  await assert.rejects(api.action("send", {}, "a"), error => error instanceof GatewayError && error.code === "uncertain" && error.cause.message === "connection lost"); assert.equal(count, 1);
  const rejected = new GatewayApi(async () => ({ ok: false, json: async () => ({ ok: false, error: { code: "action_conflict", message: "Already answered" }, receipt: { status: "rejected" } }) }));
  await assert.rejects(rejected.action("respond", {}, "b"), error => error.code === "action_conflict" && error.receipt.status === "rejected");
});

test("opening a task is a retryable read even though its HTTP method is POST", async () => {
  const api = new GatewayApi(async () => { throw new Error("Disconnected"); });
  await assert.rejects(api.open("url-task"), error => error.code === "unavailable" && error.readOnly && /Reconnect to load/.test(error.message));
  await assert.rejects(api.action("send", {}, "stable-id"), error => error.code === "uncertain" && !error.readOnly);
});

test("an interrupted native read recovers without claiming an uncertain write", async () => {
  const api = new GatewayApi(async () => ({ ok: false, status: 503, json: async () => ({ ok: false, error: { code: "uncertain", message: "thread/queue/list disconnected after delivery" } }) }));
  await assert.rejects(api.read("queue", { threadId: "a" }), error => error.code === "unavailable" && error.readOnly && error.cause.message.includes("thread/queue/list"));
  await assert.rejects(api.open("a"), error => error.code === "unavailable" && error.readOnly);
  await assert.rejects(api.action("send", { threadId: "a", text: "Once" }), error => error.code === "uncertain" && !error.readOnly);
  const action = new GatewayError("Unknown send", "uncertain", null, undefined, false);
  const read = new GatewayError("Unknown read", "uncertain", null, undefined, true);
  const next = new GatewayError("Current catalog read", "unavailable", null, undefined, true);
  assert.equal(preserveUncertainActionError(action, next), action);
  assert.equal(preserveUncertainActionError(read, next), next);
});

function recoveryFixture(threadId = "url-task", overrides = {}) {
  const visible = { thread: null, banner: null }, calls = [], timers = new Map();
  let nextTimer = 0;
  const callbacks = {
    loadStatus: async () => { calls.push("status"); return { connection: "connected", epoch: 1 }; },
    loadCatalog: async () => { calls.push("catalog"); },
    openThread: async id => { calls.push(`open:${id}`); visible.thread = id; },
    onError: error => { visible.banner = preserveUncertainActionError(visible.banner, error); },
    onRecovered: error => { if (visible.banner === error) visible.banner = null; },
    schedule: (operation, delay) => { timers.set(++nextTimer, { operation, delay }); return nextTimer; },
    cancel: timer => timers.delete(timer),
    ...overrides,
  };
  return { recovery: new WorkspaceRecovery(callbacks, threadId), visible, calls, timers };
}

test("native connection diagnostics remain visible until a successful status recovery", async () => {
  let connected = false;
  const cause = "Initialize Codex RPC at ws://127.0.0.1:4500: Invalid Codex RPC JSON";
  const fixture = recoveryFixture("a", { loadStatus: async () => ({ connection: connected ? "connected" : "reconnecting", connectionError: connected ? null : cause, epoch: 2 }) });
  await fixture.recovery.recover();
  assert.equal(fixture.visible.banner.message, cause); assert.equal(fixture.visible.banner.readOnly, true);
  assert.equal(fixture.visible.thread, null); assert.equal(fixture.timers.size, 1);
  connected = true; await fixture.recovery.recover();
  assert.equal(fixture.visible.thread, "a"); assert.equal(fixture.visible.banner, null); assert.equal(fixture.timers.size, 0);
});

test("URL task survives startup catalog 503 and clears its banner after connected recovery", async () => {
  const failed = new GatewayError("Server is reconnecting", "unavailable");
  let catalogAttempts = 0, epoch = 1;
  const fixture = recoveryFixture("url-task", {
    loadStatus: async () => ({ connection: "connected", epoch }),
    loadCatalog: async () => { if (++catalogAttempts === 1) throw failed; },
  });
  await fixture.recovery.recover();
  assert.equal(fixture.visible.thread, null); assert.equal(fixture.visible.banner, failed);
  assert.equal(fixture.recovery.threadId, "url-task"); assert.equal(fixture.timers.size, 1);
  epoch = 2;
  await fixture.recovery.recover({ catalog: true, thread: true });
  assert.equal(fixture.visible.thread, "url-task"); assert.equal(fixture.visible.banner, null);
  assert.equal(catalogAttempts, 2); assert.equal(fixture.timers.size, 0);
});

test("failed initial open retries its remembered task even when the catalog already loaded", async () => {
  let attempts = 0;
  const fixture = recoveryFixture("url-task", {
    openThread: async id => { if (++attempts === 1) throw new GatewayError("Read interrupted", "unavailable"); fixture.visible.thread = id; },
  });
  await fixture.recovery.recover();
  assert.equal(fixture.recovery.catalogReady, true); assert.equal(fixture.recovery.threadPending, true);
  await fixture.recovery.recover();
  assert.equal(fixture.visible.thread, "url-task"); assert.equal(fixture.visible.banner, null);
  assert.deepEqual(fixture.calls, ["status", "catalog", "status"]); assert.equal(attempts, 2);
});

test("a newer selection supersedes an in-flight URL read without reviving the old task", async () => {
  let release;
  const firstRead = new Promise(resolve => { release = resolve; });
  const attempted = [], fixture = recoveryFixture("url-task", {
    openThread: async id => {
      attempted.push(id);
      if (id === "url-task") { await firstRead; throw new GatewayError("Old task read failed", "unavailable"); }
      fixture.visible.thread = id;
    },
  });
  const loading = fixture.recovery.recover();
  while (!attempted.length) await Promise.resolve();
  fixture.recovery.select("newer-task"); release(); await loading;
  assert.deepEqual(attempted, ["url-task", "newer-task"]);
  assert.equal(fixture.visible.thread, "newer-task"); assert.equal(fixture.visible.banner, null); assert.equal(fixture.timers.size, 0);
});

test("a connected transition arriving during a failed read triggers recovery without a lost wakeup", async () => {
  let release, attempts = 0;
  const firstRead = new Promise(resolve => { release = resolve; });
  const fixture = recoveryFixture("url-task", {
    loadCatalog: async () => { if (++attempts === 1) { await firstRead; throw new GatewayError("Old epoch unavailable", "unavailable"); } },
  });
  const loading = fixture.recovery.recover();
  while (!attempts) await Promise.resolve();
  fixture.recovery.recover({ catalog: true, thread: true }); release(); await loading;
  assert.equal(attempts, 2); assert.equal(fixture.visible.thread, "url-task"); assert.equal(fixture.visible.banner, null);
});

test("read recovery preserves a newer uncertain-action banner and stops retrying permanent refusals", async () => {
  let attempts = 0;
  const fixture = recoveryFixture("url-task", {
    loadCatalog: async () => { if (++attempts === 1) throw new GatewayError("Read failed while reconnecting", "unavailable", null, undefined, true); },
  });
  const uncertain = new GatewayError("Send may have been accepted", "uncertain");
  fixture.visible.banner = uncertain;
  await fixture.recovery.recover();
  assert.equal(fixture.visible.thread, null); assert.equal(fixture.visible.banner, uncertain);
  assert.equal(fixture.recovery.error.code, "unavailable"); assert.equal(fixture.timers.size, 1);
  await fixture.recovery.recover();
  assert.equal(fixture.visible.thread, "url-task"); assert.equal(fixture.visible.banner, uncertain);
  fixture.recovery.openThread = async () => { throw new GatewayError("Task is outside allowed roots", "invalid_input"); };
  await fixture.recovery.select("forbidden-task");
  assert.equal(fixture.visible.banner.code, "invalid_input"); assert.equal(fixture.timers.size, 0);
});

test("history consumes native opaque cursors and detects a repeated cursor", async () => {
  const calls = []; const api = { read: async (route, query) => { calls.push(query.cursor); return query.cursor ? { data: [2], nextCursor: null } : { data: [1], nextCursor: "opaque/+=" }; } };
  assert.deepEqual(await readAllPages(api, "turns", { threadId: "t" }), [1, 2]); assert.deepEqual(calls, [null, "opaque/+="]);
  await assert.rejects(readAllPages({ read: async () => ({ data: [], nextCursor: "repeat" }) }, "turns"), /repeated/);
});

test("failed runtime recovery closes the affected task and preserves its visible error", () => {
  const state = reduceEvent(snapshot(), event("gateway/recoveryError", { threadId: "thread-a", message: "Directory moved outside allowed roots" }));
  assert.equal(state.thread, null); assert.deepEqual(state.pending, []); assert.deepEqual(state.queue, []);
  assert.equal(state.lastError, "Directory moved outside allowed roots");
});

test("accepted native errors report once without discarding the selected task", () => {
  const errors = [], value = event("error", { threadId: "thread-a", error: { message: "Native failure" } });
  const apply = (state, event) => applyWorkspaceEvent(state, event, reduceEvent, () => assert.fail("A native error does not forget the task"), message => errors.push(message));
  let state = apply(snapshot(), value);
  state = apply(state, value);
  assert.equal(state.thread.id, "thread-a"); assert.equal(state.lastError, "Native failure");
  assert.deepEqual(errors, ["Native failure"]);
  state = apply(state, event("error", { error: { message: "Global runtime failure" } }, { sequence: 12 }));
  assert.deepEqual(errors, ["Native failure", "Global runtime failure"]);
});

test("stale, gapped, other-epoch and other-task native errors cannot replace the banner", () => {
  const invalid = [
    event("error", { threadId: "thread-a", message: "Stale" }, { sequence: 10 }),
    event("error", { threadId: "thread-a", message: "Other epoch" }, { epoch: 4 }),
    event("error", { threadId: "thread-a", message: "Missing prior event" }, { sequence: 13 }),
    event("error", { threadId: "another-task", message: "Other task" }),
  ];
  for (const value of invalid) {
    let reports = 0;
    const initial = { ...snapshot(), lastError: "Previous failure" };
    applyWorkspaceEvent(initial, value, reduceEvent, () => assert.fail("Unrelated recovery"), () => reports++);
    assert.equal(reports, 0);
  }
});

test("persisted failed turn errors remain readable without transient runtime events", () => {
  const error = { message: "Native failed turn", additionalDetails: "Failure detail", codexErrorInfo: { other: "diagnostic" } };
  const records = threadRecords({ id: "t", turns: [{ id: "failed-turn", status: "failed", items: [], error }] });
  assert.equal(records.length, 1); assert.equal(records[0].head.target, error.message);
  assert.deepEqual(JSON.parse(records[0].body[0].x), error); assert.equal(records[0].runtime.running, false);
  assert.equal(threadRecords({ id: "t", turns: [{ id: "ok-turn", status: "completed", items: [], error: null }] }).length, 0);
});

test("buffered recovery failure applies banner and selection effects exactly once after a snapshot", async () => {
  const fixture = recoveryFixture("thread-a");
  const effects = [];
  const apply = (state, value) => applyWorkspaceEvent(state, value, reduceEvent, (id, message) => {
    effects.push(id); fixture.recovery.forget(id); fixture.visible.banner = message;
  });
  const buffered = [event("gateway/recoveryError", { threadId: "thread-a", message: "Directory moved outside allowed roots" })];
  let state = snapshot();
  for (const value of buffered) state = apply(state, value);
  // openThread's finally path replays the buffer even after a successful snapshot replay.
  for (const value of buffered) state = apply(state, value);
  assert.equal(state.thread, null); assert.equal(fixture.visible.banner, "Directory moved outside allowed roots");
  assert.equal(fixture.recovery.threadId, null); assert.equal(fixture.recovery.threadPending, false);
  assert.deepEqual(effects, ["thread-a"]);
  await fixture.recovery.recover({ thread: true });
  assert.equal(fixture.calls.some(call => call.startsWith("open:")), false);
  assert.equal(fixture.visible.banner, "Directory moved outside allowed roots");
});

test("stale, other-epoch, gap and unrelated recovery errors cannot clear the requested selection", () => {
  const invalid = [
    event("gateway/recoveryError", { threadId: "thread-a", message: "Stale" }, { sequence: 10 }),
    event("gateway/recoveryError", { threadId: "thread-a", message: "Other epoch" }, { epoch: 4 }),
    event("gateway/recoveryError", { threadId: "thread-a", message: "Missing prior event" }, { sequence: 13 }),
    event("gateway/recoveryError", { threadId: "other-task", message: "Another task" }),
  ];
  for (const value of invalid) {
    const fixture = recoveryFixture("thread-a");
    const next = applyWorkspaceEvent(snapshot(), value, reduceEvent, (id, message) => { fixture.recovery.forget(id); fixture.visible.banner = message; });
    assert.equal(next.thread.id, "thread-a"); assert.equal(fixture.recovery.threadId, "thread-a"); assert.equal(fixture.visible.banner, null);
  }
});

test("PWA declares standalone scope and its cache excludes private endpoints", async () => {
  const manifest = JSON.parse(await readFile(new URL("../src/codex-workspace/manifest.webmanifest", import.meta.url), "utf8"));
  assert.equal(manifest.display, "standalone"); assert.equal(manifest.scope, "/");
  for (const size of [192, 512]) {
    const icon = manifest.icons.find(icon => icon.sizes === `${size}x${size}`); assert.ok(icon);
    const bytes = await readFile(new URL(`../src${icon.src}`, import.meta.url));
    assert.equal(bytes.readUInt32BE(16), size); assert.equal(bytes.readUInt32BE(20), size);
  }
  const source = await readFile(new URL("../src/codex-workspace/sw.js", import.meta.url), "utf8");
  assert.match(source, /event\.request\.method !== "GET"/); assert.match(source, /url\.search/); assert.doesNotMatch(source, /"\/api\/|"\/pair|"\/file/);
});


test("inline queue images have a short label while existing remote/path descriptors stay visible", () => {
  assert.equal(textOfInput([{ type: "image", url: "data:image/png;base64,PRIVATE_IMAGE_BYTES" }]), "[Image attached]");
  assert.equal(textOfInput([{ type: "image", url: "https://example.test/image.png" }]), "[Image: https://example.test/image.png]");
});


test("A-to-B-to-A selection cannot retire the new selection with its first stale open", async () => {
  let release, attempts = 0, generation = 0;
  const held = new Promise(resolve => { release = resolve; });
  const fixture = recoveryFixture("a", { openThread: async id => {
    const observed = generation; if (++attempts === 1) await held;
    if (observed === generation) fixture.visible.thread = id;
  } });
  const loading = fixture.recovery.recover(); while (!attempts) await Promise.resolve();
  generation++; fixture.recovery.select("b"); generation++; fixture.recovery.select("a"); release(); await loading;
  assert.equal(fixture.visible.thread, "a"); assert.equal(attempts, 2); assert.equal(fixture.recovery.threadPending, false);
});

test("catalog invalidation during loading cannot be overwritten by the old read", async () => {
  let release, reads = 0;
  const held = new Promise(resolve => { release = resolve; });
  const fixture = recoveryFixture("a", { loadCatalog: async () => { if (++reads === 1) await held; } });
  const loading = fixture.recovery.recover(); while (!reads) await Promise.resolve();
  fixture.recovery.recover({ catalog: true }); release(); await loading;
  assert.equal(reads, 2); assert.equal(fixture.visible.thread, "a"); assert.equal(fixture.recovery.catalogReady, true);
});

test("an unconfirmed snapshot retries its remembered selection without reporting ready", async () => {
  let attempts = 0, confirmed = 0;
  const fixture = recoveryFixture("a", { openThread: async id => { if (++attempts === 1) return false; fixture.visible.thread = id; }, onRecovered: () => { confirmed++; } });
  await fixture.recovery.recover(); assert.equal(fixture.recovery.threadPending, true); assert.equal(confirmed, 0); assert.equal(fixture.timers.size, 1);
  await fixture.recovery.recover(); assert.equal(fixture.visible.thread, "a"); assert.equal(confirmed, 1); assert.equal(attempts, 2); assert.equal(fixture.timers.size, 0);
});

for (const stage of ["headers", "body"]) for (const read of [true, false]) test(`Gateway ${read ? "read" : "action"} deadline bounds stalled ${stage} and preserves outcome classification`, async () => {
  let expire, cancelled = 0, signal, decoding = false;
  const held = new Promise(() => {});
  const api = new GatewayApi(async (_url, request) => { signal = request.signal; if (stage === "headers") return held; return { ok: true, json() { decoding = true; return held; } }; }, { schedule(operation) { expire = operation; return 1; }, cancel() { cancelled++; } });
  const pending = read ? api.open("a") : api.action("send", { threadId: "a", text: "One action" }, "stable-id");
  if (stage === "body") while (!decoding) await Promise.resolve();
  const outcome = assert.rejects(pending, error => error.code === (read ? "unavailable" : "uncertain") && error.readOnly === read && error.cause.message.includes("/api/codex/"));
  expire(); await outcome; assert.equal(signal.aborted, true); assert.equal(cancelled, 1);
});

test("a Monitor index body stall is a bounded read and normal success cancels its deadline", async () => {
  let expire, cancelled = 0, decoding = false;
  const urls = [], api = new GatewayApi(async (url, request) => {
    urls.push(url); assert.equal(request.method, "GET"); assert.equal(request.credentials, "same-origin");
    return { ok: true, json() { decoding = true; return new Promise(() => {}); } };
  }, { schedule(operation) { expire = operation; return 1; }, cancel() { cancelled++; } });
  const loading = api.readMonitorIndex(); while (!decoding) await Promise.resolve();
  const outcome = assert.rejects(loading, error => error.code === "unavailable" && error.readOnly); expire(); await outcome;
  assert.deepEqual(urls, ["/api/sessions"]); assert.equal(cancelled, 1);
  api.fetcher = async () => ({ ok: true, json: async () => ({ groups: [] }) });
  assert.deepEqual(await api.readMonitorIndex(), { groups: [] }); assert.equal(cancelled, 2);
});

test("same-thread refresh requested during an open is confirmed by a fresh read", async () => {
  let release, opens = 0, recovered = 0;
  const held = new Promise(resolve => { release = resolve; });
  const fixture = recoveryFixture("a", { openThread: async () => { const attempt = ++opens; if (attempt === 1) await held; fixture.visible.thread = attempt === 1 ? "obsolete" : "current"; }, onRecovered() { recovered++; } });
  const loading = fixture.recovery.recover(); while (!opens) await Promise.resolve();
  fixture.recovery.recover({ thread: true }); release(); await loading;
  assert.equal(opens, 2); assert.equal(fixture.visible.thread, "current"); assert.equal(recovered, 1); assert.equal(fixture.recovery.threadPending, false);
});

for (const path of ["status", "open", "index"]) test(`plain-text 401 on ${path} is an explicit pairing refusal without automatic retry`, async () => {
  let calls = 0;
  const api = new GatewayApi(async () => { calls++; return new Response("Authentication required", { status: 401, headers: { "Content-Type": "text/plain" } }); });
  const operation = () => path === "index" ? api.readMonitorIndex() : path === "open" ? api.open("a") : api.read("status");
  await assert.rejects(operation(), error => error.code === "pairing_required" && error.readOnly && /Pair this browser/.test(error.message));
  const fixture = recoveryFixture("a", { loadStatus: operation });
  await fixture.recovery.recover(); assert.equal(fixture.visible.banner.code, "pairing_required"); assert.equal(fixture.timers.size, 0); assert.equal(calls, 2);
});
