import assert from "node:assert/strict";

/** Exercise stale request controls, question drafts, and recovery in the real app module. */
export async function assertRequestGenerationRecovery(page) {
  const context = await page.context().browser().newContext({ storageState: await page.context().storageState(), serviceWorkers: "block" });
  const fixturePage = await context.newPage(), errors = [], responses = [];
  const threadId = "request-generation-fixture", thread = { id: threadId, cwd: "/fixture", name: "Request generation fixture", turns: [] };
  let epoch = 3, sequence = 10, pending = [], opens = 0, blockStatus = false, streamTimer;
  let streamDelivery = Promise.resolve();
  const statusWaiters = [];
  const releaseStatus = () => { blockStatus = false; for (const resolve of statusWaiters.splice(0)) resolve(); };
  const checks = {};
  fixturePage.on("pageerror", error => errors.push(error.message));
  await fixturePage.addInitScript(() => {
    window.EventSource = class { constructor() { window.requestGenerationEvents = this; } close() {} };
  });
  await context.route("**/api/codex/**", async route => {
    const name = new URL(route.request().url()).pathname.split("/").at(-1);
    let value = { ok: true, data: [], nextCursor: null };
    if (name === "status") {
      if (blockStatus) await new Promise(resolve => statusWaiters.push(resolve));
      value = { ok: true, connection: "connected", epoch, sequence, allowedRoots: ["/fixture"] };
    } else if (name === "models") value.data = [{ id: "fixture-model", isDefault: true, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low"] }];
    else if (name === "threads") value.data = new URL(route.request().url()).searchParams.get("archived") === "true" ? [] : [thread];
    else if (name === "open") {
      opens++;
      value = { ok: true, snapshot: { thread, settings: { model: "fixture-model", reasoningEffort: "low" }, pending, queue: [], connection: "connected", epoch, sequence } };
    } else if (name === "respond") {
      responses.push(route.request().postDataJSON()); value = { ok: true, delivered: true };
    } else if (name === "render") value = { ok: true, html: route.request().postDataJSON().texts.map(() => "") };
    else assert.equal(name, "projects", `Only isolated fixture reads and answers are expected, received ${name}`);
    await route.fulfill({ json: value });
  });
  const emit = async (method, params, extras = {}) => {
    const event = { epoch, sequence: ++sequence, method, params, requestId: null, request: null, ...extras };
    await fixturePage.evaluate(event => window.requestGenerationEvents.onmessage({ data: JSON.stringify(event) }), event);
  };
  const question = requestToken => ({ id: 7, requestToken, canRespond: true, method: "item/tool/requestUserInput", params: { threadId, questions: [{ id: "answer", header: "Question", question: "Choose", options: [{ label: "First", description: "First option" }] }] } });
  const approval = (id, requestToken) => ({ id, requestToken, canRespond: true, method: "item/commandExecution/requestApproval", params: { threadId, command: "printf isolated", availableDecisions: ["accept", "decline"] } });
  const publish = async request => {
    pending = [...pending.filter(value => !(typeof value.id === typeof request.id && value.id === request.id)), request];
    await emit(request.method, request.params, { requestId: request.id, request });
  };
  try {
    await fixturePage.goto(`${new URL(page.url()).origin}/codex-workspace?thread=${threadId}`);
    await fixturePage.waitForFunction(() => !document.getElementById("model-select").disabled);
    const sourceSha256 = await fixturePage.evaluate(async () => Object.fromEntries(await Promise.all(["index.html", "app.js", "model.js", "requests.js"].map(async name => {
      const path = `/codex-workspace/${name}`, response = await fetch(path, { cache: "no-store" });
      if (!response.ok) throw new Error(`Cannot identify fixture UI source ${path}: ${response.status}`);
      const digest = await crypto.subtle.digest("SHA-256", await response.arrayBuffer());
      return [path, [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("")];
    }))));
    await publish(question("instance-a:3:11"));
    await fixturePage.locator('[data-request-token="instance-a:3:11"]').waitFor();
    await fixturePage.locator("[data-free-answer]").fill("OLD_QUESTION_DRAFT");
    await fixturePage.getByTestId("composer-input").fill("force render");
    await fixturePage.waitForTimeout(40);
    assert.equal(await fixturePage.locator("[data-free-answer]").inputValue(), "OLD_QUESTION_DRAFT");
    checks.sameTokenQuestionDraftSurvivesRender = true;
    await publish(question("instance-a:3:11"));
    await fixturePage.waitForTimeout(40);
    assert.equal(await fixturePage.locator("[data-free-answer]").inputValue(), "OLD_QUESTION_DRAFT");
    checks.identicalRequestReplayPreservesQuestionDraft = true;
    const refreshed = fixturePage.waitForResponse(response => new URL(response.url()).pathname === "/api/codex/open");
    await fixturePage.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await (await refreshed).finished();
    await fixturePage.waitForFunction(() => !document.getElementById("model-select").disabled);
    assert.equal(await fixturePage.locator("[data-free-answer]").inputValue(), "OLD_QUESTION_DRAFT");
    checks.backgroundSnapshotPreservesQuestionDraft = true;
    await publish(question("instance-a:3:12"));
    await fixturePage.locator('[data-request-token="instance-a:3:12"]').waitFor();
    assert.equal(await fixturePage.locator("[data-free-answer]").inputValue(), "");
    checks.replacedNativeIdCannotInheritQuestionDraft = true;
    await fixturePage.locator("[data-free-answer]").fill("NEW_ANSWER");
    await fixturePage.getByTestId("submit-answer").click();
    await fixturePage.waitForTimeout(80);
    assert.equal(responses.at(-1)?.requestToken, "instance-a:3:12");
    assert.equal(responses.at(-1)?.threadId, threadId); assert.equal(responses.at(-1)?.requestId, 7);
    assert.deepEqual(responses.at(-1)?.response, { answers: { answer: { answers: ["NEW_ANSWER"] } } });
    checks.questionAnswerRetainsNativeRequestGeneration = true;
    pending = []; await emit("serverRequest/resolved", { threadId, requestId: 7 });
    await publish(approval(7, "instance-a:3:14"));
    await fixturePage.locator('[data-request-token="instance-a:3:14"]').waitFor();
    await fixturePage.evaluate(() => { window.requestGenerationOldControl = document.querySelector('[data-request-token="instance-a:3:14"]').cloneNode(true); });
    await publish(approval(7, "instance-a:3:15"));
    await fixturePage.locator('[data-request-token="instance-a:3:15"]').waitFor();
    const beforeOld = responses.length;
    const clickOldControl = () => fixturePage.evaluate(() => {
      document.getElementById("pending-requests").append(window.requestGenerationOldControl);
      window.requestGenerationOldControl.querySelector('[data-decision="accept"]').click();
      window.requestGenerationOldControl.remove();
    });
    await clickOldControl(); await fixturePage.waitForTimeout(80);
    assert.equal(responses.length, beforeOld);
    checks.staleApprovalDomCannotAnswerReplacement = true;
    await publish(approval("7", "instance-a:3:16"));
    await fixturePage.locator('[data-request-token="instance-a:3:16"]').waitFor();
    await fixturePage.locator('[data-request-token="instance-a:3:16"] [data-decision="decline"]').click();
    await fixturePage.waitForTimeout(80);
    assert.equal(responses.at(-1)?.requestId, "7"); assert.equal(responses.at(-1)?.requestToken, "instance-a:3:16");
    checks.nativeNumberAndStringIdsRemainDistinctInDom = true;
    blockStatus = true; sequence += 4;
    await emit("item/agentMessage/delta", { threadId, itemId: "missing", turnId: "missing", delta: "gap" });
    await fixturePage.waitForFunction(() => document.querySelectorAll("[data-request-token]").length === 0);
    assert.equal(await fixturePage.getByTestId("send-message").isDisabled(), true);
    assert.equal(await fixturePage.getByTestId("model-select").isDisabled(), true);
    checks.sequenceGapPausesWritersAndClearsRequests = true;
    const beforeGap = responses.length;
    await clickOldControl(); await fixturePage.waitForTimeout(80);
    assert.equal(responses.length, beforeGap);
    checks.staleApprovalCannotDispatchDuringRecovery = true;
    pending = [approval(7, "instance-a:3:21")]; releaseStatus();
    await fixturePage.waitForFunction(() => !document.getElementById("model-select").disabled);
    await fixturePage.locator('[data-request-token="instance-a:3:21"]').waitFor();
    checks.freshSnapshotRestoresCurrentRequestWriters = true;
    blockStatus = true; epoch = 4; sequence = 0;
    await emit("gateway/connection", { connection: "connected" });
    await fixturePage.waitForFunction(() => document.querySelectorAll("[data-request-token]").length === 0);
    assert.equal(await fixturePage.getByTestId("send-message").isDisabled(), true);
    assert.equal(await fixturePage.getByTestId("model-select").isDisabled(), true);
    pending = [approval(7, "instance-a:4:2")]; releaseStatus();
    await fixturePage.locator('[data-request-token="instance-a:4:2"]').waitFor();
    await fixturePage.waitForFunction(() => !document.getElementById("model-select").disabled);
    checks.nativeReconnectRequiresNewRequestSnapshot = true;
    pending = []; thread.turns = [{ id: "streaming", status: "inProgress", items: [] }];
    const beforeStream = opens;
    await emit("turn/started", { threadId, turn: thread.turns[0] });
    // These deltas arrive faster than the recovery delay. A debounce would
    // postpone every snapshot and keep Stop disabled until streaming finished.
    streamTimer = setInterval(() => {
      streamDelivery = streamDelivery.then(() => emit("item/agentMessage/delta", { threadId, turnId: "streaming", itemId: "continuous", delta: "delta " }));
    }, 20);
    await fixturePage.waitForTimeout(700);
    assert(opens > beforeStream, "Continuous deltas must not indefinitely postpone required snapshot recovery");
    assert.equal(await fixturePage.getByTestId("interrupt-turn").isDisabled(), false, "Stop becomes usable during continuous streaming after the required snapshot");
    checks.continuousStreamingCannotStarveSnapshotOrStop = true;
    assert.deepEqual(errors, []);
    return { checks, sourceSha256, responseCount: responses.length, openCount: opens };
  } finally {
    clearInterval(streamTimer); releaseStatus(); await streamDelivery;
    await context.close();
  }
}

/** Exercise the actual app's delayed-open buffer with mocked reads and no native actions. */
export async function assertBufferedRecoveryError(page) {
  const context = await page.context().browser().newContext({ storageState: await page.context().storageState(), serviceWorkers: "block" });
  const fixturePage = await context.newPage();
  const errors = [], threadId = "buffered-recovery-fixture", message = "This task moved outside the allowed project roots.";
  const status = { ok: true, connection: "connected", epoch: 1, sequence: 10, allowedRoots: ["/fixture"] };
  let opens = 0, releaseOpen, signalOpen;
  const opened = new Promise(resolve => { signalOpen = resolve; });
  const released = new Promise(resolve => { releaseOpen = resolve; });
  fixturePage.on("pageerror", error => errors.push(error.message));
  await fixturePage.addInitScript(() => {
    window.EventSource = class {
      constructor() { window.bufferedRecoveryEvents = this; }
      close() {}
    };
  });
  await context.route("**/api/codex/**", async route => {
    const name = new URL(route.request().url()).pathname.split("/").at(-1);
    let value = { ok: true, data: [], nextCursor: null };
    if (name === "status") value = status;
    else if (name === "models") value.data = [{ id: "fixture-model", isDefault: true, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low"] }];
    else if (name === "open") {
      opens++; signalOpen(); await released;
      value = { ok: true, snapshot: { thread: { id: threadId, cwd: "/fixture", turns: [] }, settings: { model: "fixture-model", effort: "low" }, pending: [], queue: [], connection: "connected", epoch: 1, sequence: 10 } };
    } else assert(["projects", "threads"].includes(name), `Only workspace reads are expected, received ${name}`);
    await route.fulfill({ json: value });
  });
  try {
    const base = new URL(page.url()).origin;
    await fixturePage.goto(`${base}/codex-workspace?thread=${threadId}`);
    await opened;
    await fixturePage.evaluate(({ threadId, message }) => {
      window.bufferedRecoveryEvents.onmessage({ data: JSON.stringify({ epoch: 1, sequence: 11, method: "gateway/recoveryError", params: { threadId, message } }) });
    }, { threadId, message });
    releaseOpen();
    await fixturePage.waitForFunction(expected => document.getElementById("workspace-error").textContent === expected && !document.getElementById("workspace-error").hidden, message);
    assert.equal(await fixturePage.locator("#thread-title").innerText(), "Codex Workspace");
    assert.equal(await fixturePage.locator("#composer-input").isDisabled(), true);
    assert.equal(new URL(fixturePage.url()).searchParams.has("thread"), false);
    await fixturePage.evaluate(() => window.bufferedRecoveryEvents.onmessage({ data: JSON.stringify({ epoch: 1, sequence: 12, method: "gateway/historyChanged", params: {} }) }));
    await fixturePage.waitForTimeout(300);
    assert.equal(opens, 1, "A cleared selection must not be implicitly reopened after buffered failure.");
    assert.equal(await fixturePage.locator("#workspace-error").innerText(), message);
    assert.deepEqual(errors, []);
    return { bufferedRecoveryErrorVisible: true, failedSelectionCleared: true, noImplicitReopen: true };
  } finally { releaseOpen(); await context.close(); }
}

/** Native failure notifications must reach the real UI, both live and during snapshot replay. */
export async function assertRuntimeErrorReporting(page) {
  const context = await page.context().browser().newContext({ storageState: await page.context().storageState(), serviceWorkers: "block" });
  const fixturePage = await context.newPage();
  const errors = [], threadId = "runtime-error-fixture";
  let releaseOpen, signalOpen, persistedFailure = false;
  const opened = new Promise(resolve => { signalOpen = resolve; });
  const released = new Promise(resolve => { releaseOpen = resolve; });
  fixturePage.on("pageerror", error => errors.push(error.message));
  await fixturePage.addInitScript(() => {
    window.EventSource = class { constructor() { window.runtimeErrorEvents = this; } close() {} };
  });
  await context.route("**/api/codex/**", async route => {
    const name = new URL(route.request().url()).pathname.split("/").at(-1);
    let value = { ok: true, data: [], nextCursor: null };
    if (name === "status") value = { ok: true, connection: "connected", epoch: 1, sequence: 10, allowedRoots: ["/fixture"] };
    else if (name === "models") value.data = [{ id: "fixture-model", isDefault: true, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low"] }];
    else if (name === "open") {
      signalOpen(); await released;
      const turn = persistedFailure ? { id: "failed-turn", status: "failed", items: [], error: { message: "PERSISTED_ERROR_SENTINEL", additionalDetails: "Retained native failure details" } } : { id: "active-turn", status: "inProgress", items: [] };
      value = { ok: true, snapshot: { thread: { id: threadId, cwd: "/fixture", name: "Native error fixture", turns: [turn] }, settings: { model: "fixture-model", effort: "low" }, pending: [], queue: [], connection: "connected", epoch: 1, sequence: 10 } };
    } else assert(["projects", "threads"].includes(name), `Only workspace reads are expected, received ${name}`);
    await route.fulfill({ json: value });
  });
  const emit = (message, sequence, id = threadId) => fixturePage.evaluate(({ message, sequence, id }) => {
    window.runtimeErrorEvents.onmessage({ data: JSON.stringify({ epoch: 1, sequence, method: "error", params: { threadId: id, error: { message } } }) });
  }, { message, sequence, id });
  try {
    await fixturePage.goto(`${new URL(page.url()).origin}/codex-workspace?thread=${threadId}`);
    await opened;
    await emit("BUFFERED_ERROR_SENTINEL", 11); releaseOpen();
    await fixturePage.waitForFunction(() => !document.getElementById("workspace-error").hidden && document.getElementById("workspace-error").textContent === "BUFFERED_ERROR_SENTINEL");
    assert.equal(await fixturePage.locator("#composer-input").isDisabled(), false);
    assert.equal(new URL(fixturePage.url()).searchParams.get("thread"), threadId);
    await fixturePage.locator("#composer-input").fill("Keep this draft");
    await emit("A duplicate must not replace the error", 11);
    await emit("Another task must not replace the error", 12, "another-thread");
    assert.equal(await fixturePage.locator("#workspace-error").innerText(), "BUFFERED_ERROR_SENTINEL");
    const liveError = "LIVE_ERROR_SENTINEL <script>unsafe()</script>";
    await emit(liveError, 13);
    await fixturePage.waitForFunction(expected => !document.getElementById("workspace-error").hidden && document.getElementById("workspace-error").textContent === expected, liveError);
    assert.equal(await fixturePage.locator("#workspace-error script").count(), 0);
    assert.equal(await fixturePage.locator("#composer-input").inputValue(), "Keep this draft");
    persistedFailure = true;
    await fixturePage.reload();
    await fixturePage.waitForFunction(() => document.getElementById("transcript").innerText.includes("PERSISTED_ERROR_SENTINEL"));
    assert.deepEqual(errors, []);
    return { bufferedNativeErrorVisible: true, liveNativeErrorVisible: true, nativeErrorPreservesSelectionAndDraft: true, failedTurnErrorSurvivesReload: true };
  } finally { releaseOpen(); await context.close(); }
}

/** Actual browser DOM contract for the native adapter and the existing Monitor reader. */
export async function assertMonitorAdapterRendering(page) {
  const result = await page.evaluate(async () => {
    const [{ threadRecords }, { Projection }, { renderUnit }, { GatewayApi }] = await Promise.all([
      import("/codex-workspace/model.js"), import("/monitor-ui/view-model.js"),
      import("/monitor-ui/components.js"), import("/codex-workspace/api.js"),
    ]);
    // Uses the real Window fetch implementation, catching detached-fetch receiver failures.
    const api = new GatewayApi();
    const status = await api.read("status");
    const rendered = await api.request("render", { texts: ["Render fixture prompt", "Render fixture complete"] });
    const html = new Map([["Render fixture prompt", rendered.html[0]], ["Render fixture complete", rendered.html[1]]]);
    const patch = "--- a/file.txt\n+++ b/file.txt\n@@ -3,2 +3,2 @@\n-old value\n+new value\n stable value\n\\ No newline at end of file";
    const unknown = { id: "future-a", type: "futureNativeItem", payload: { note: "Unknown item remains readable", text: "<script>unsafe()</script>" } };
    const native = { id: "renderer-thread", cwd: "/fixture/project", turns: [{ id: "renderer-turn", status: "completed", items: [
      { id: "user-a", type: "userMessage", content: [{ type: "text", text: "Render fixture prompt" }] },
      { id: "command-a", type: "commandExecution", status: "completed", command: "printf fixture-output", cwd: "/fixture/project/subdirectory", aggregatedOutput: "fixture-output\n", exitCode: 0 },
      { id: "edit-a", type: "fileChange", status: "completed", changes: [{ path: "/fixture/project/file.txt", kind: { type: "update", move_path: null }, diff: patch }] },
      unknown,
      { id: "answer-a", type: "agentMessage", text: "Render fixture complete", phase: "final_answer" },
    ] }] };
    const equivalent = [
      { id: "user-a", kind: "user", turn: 1, src: "Render fixture prompt", body: [{ p: "md", h: html.get("Render fixture prompt") }] },
      { id: "command-a", kind: "bash", tool: "command", turn: 1, head: { name: "Command", target: "printf fixture-output", chips: [{ x: "completed", c: "state" }, { x: "exit 0", c: "state" }] }, body: [{ p: "note", x: "Directory: /fixture/project/subdirectory" }, { p: "pre", x: "fixture-output\n" }] },
      { id: "edit-a", kind: "edit", tool: "fileChange", turn: 1, head: { name: "File changes", target: "/fixture/project/file.txt", chips: [{ x: "completed", c: "state" }] }, body: [{ p: "note", x: "/fixture/project/file.txt" }, { p: "diff", rows: [["ctx", null, "--- a/file.txt"], ["ctx", null, "+++ b/file.txt"], ["ctx", null, "@@ -3,2 +3,2 @@"], ["del", 3, "old value"], ["add", 3, "new value"], ["ctx", 4, "stable value"], ["ctx", null, "\\ No newline at end of file"]] }] },
      { id: "future-a", kind: "tool", turn: 1, head: { name: "futureNativeItem", chips: [{ x: "completed", c: "state" }] }, body: [{ p: "pre", x: JSON.stringify(unknown, null, 2) }] },
      { id: "answer-a", kind: "assistant", phase: "final", turn: 1, src: "Render fixture complete", body: [{ p: "md", h: html.get("Render fixture complete") }] },
    ];
    const makeState = mode => ({ folds: new Map(mode === "expanded" ? [["command-a", false], ["edit-a", false], ["future-a", false]] : []), processFolds: new Map(), processExpanded: new Set(), processChosen: new Set(), processBulk: new Map(), promptExpanded: new Set(), rawTurns: new Map(), fullTargets: new Set(), headSteps: new Map(), capOpen: new Set(), openImages: new Set(), codeOverrides: new Map() });
    const container = document.createElement("section");
    container.style.cssText = "position:absolute;left:-9999px;top:0;width:800px";
    document.body.append(container);
    try {
      const modes = {};
      for (const mode of ["default", "expanded"]) {
        const views = [threadRecords(native, text => html.get(text)), equivalent].map(records => {
        const projection = new Projection(); projection.rebuild(records);
        projection.rebuild(records, 1); // A streamed process rewrite must not duplicate the prompt or action.
        const root = document.createElement("div"); root.append(...projection.units.map(unit => renderUnit(unit, makeState(mode)))); container.append(root);
        return {
          text: root.innerText, users: root.querySelectorAll(".turn.user").length,
          answers: root.querySelectorAll(".turn.final-answer").length,
          commands: root.querySelectorAll('[data-record-id="command-a"]').length,
          edits: root.querySelectorAll('[data-record-id="edit-a"]').length,
          scripts: root.querySelectorAll("script").length,
          closedRenderers: root.querySelectorAll("[data-renderer].closed").length,
          commandBodyVisible: !!root.querySelector('[data-record-id="command-a"] .renderer-body')?.getClientRects().length,
          added: [...root.querySelectorAll(".line.add")].map(line => ({ mark: line.querySelector(".mark")?.textContent, number: line.querySelector(".ln")?.textContent, text: line.querySelector(".codecell")?.textContent })),
          deleted: [...root.querySelectorAll(".line.del")].map(line => ({ mark: line.querySelector(".mark")?.textContent, number: line.querySelector(".ln")?.textContent, text: line.querySelector(".codecell")?.textContent })),
          headers: [...root.querySelectorAll(".line:not(.add):not(.del) .codecell")].map(line => line.textContent),
        };
        });
        modes[mode] = { native: views[0], monitor: views[1] };
      }
      return { connection: status.connection, ...modes };
    } finally { container.remove(); }
  });
  assert.equal(result.connection, "connected");
  for (const mode of ["default", "expanded"]) assert.deepEqual(result[mode].native, result[mode].monitor, `Native and equivalent Monitor records render the same ${mode} reader content.`);
  assert.equal(result.default.native.closedRenderers, 3); assert.equal(result.default.native.commandBodyVisible, false);
  const native = result.expanded.native;
  assert.equal(native.closedRenderers, 0); assert.equal(native.commandBodyVisible, true);
  assert.equal(native.users, 1); assert.equal(native.answers, 1);
  assert.equal(native.commands, 1); assert.equal(native.edits, 1); assert.equal(native.scripts, 0);
  assert.match(native.text, /fixture-output/); assert.match(native.text, /Directory: \/fixture\/project\/subdirectory/);
  assert.match(native.text, /Unknown item remains readable/);
  assert.deepEqual(native.added, [{ mark: "+", number: "3", text: "new value" }]);
  assert.deepEqual(native.deleted, [{ mark: "−", number: "3", text: "old value" }]);
  assert(native.headers.includes("--- a/file.txt")); assert(native.headers.includes("+++ b/file.txt")); assert(native.headers.includes("@@ -3,2 +3,2 @@"));
  return { commandOutputAndDirectory: true, fileDiffHeadersSignsAndLines: true, unknownReadableWithoutActiveContent: true, noDuplicatePromptsOrActions: true, monitorRendererEquivalent: true, defaultAndExpandedModesEquivalent: true, realBrowserFetchReceiver: true, ...await assertUnknownInputRendering(page), ...await assertCommandExplorationRendering(page), ...await assertPreservedNativeFactsRendering(page) };
}

/** Native plan notifications and opaque historical tool facts retain the shared reader's content. */
async function assertPreservedNativeFactsRendering(page) {
  const result = await page.evaluate(async () => {
    const [{ threadRecords, freshWorkspace, applySnapshot, reduceEvent }, { Projection }, { renderUnit }] = await Promise.all([
      import("/codex-workspace/model.js"), import("/monitor-ui/view-model.js"), import("/monitor-ui/components.js"),
    ]);
    const markup = '<img src=x onerror="window.preservedFactsExecuted=1"><script>window.preservedFactsExecuted=1</script>';
    const explanation = `PLAN_EXPLANATION_SENTINEL ${markup}`;
    const legacyOutput = `LEGACY_CHANGE_OUTPUT_SENTINEL ${markup}`;
    const searchTitle = "OPAQUE_RESULT_TITLE_SENTINEL";
    const searchUrl = "https://example.invalid/opaque-reference";
    const searchSnippet = `OPAQUE_RESULT_SNIPPET_SENTINEL ${markup}`;
    const nativeThread = { id: "preservation-thread", cwd: "/fixture", turns: [{ id: "preservation-turn", status: "completed", items: [
      { id: "empty-add", type: "fileChange", status: "completed", changes: [{ path: "/fixture/empty-added.txt", kind: { type: "add" }, diff: "" }] },
      { id: "empty-delete", type: "fileChange", status: "completed", changes: [{ path: "/fixture/empty-deleted.txt", kind: { type: "delete" }, diff: "" }] },
      { id: "moved-file", type: "fileChange", status: "completed", changes: [{ path: "/fixture/original.txt", kind: { type: "update", move_path: "/fixture/moved-destination.txt" }, diff: "" }] },
      { id: "legacy-change", type: "fileChange", status: "completed", aggregatedOutput: legacyOutput },
      { id: "opaque-search", type: "webSearch", status: "completed", action: { type: "search", query: "Opaque search query" }, results: [{ title: searchTitle, url: searchUrl, snippet: searchSnippet }] },
    ] }] };
    const plan = { threadId: "preservation-thread", turnId: "preservation-turn", explanation, plan: [
      { step: "COMPLETED_STEP_SENTINEL", status: "completed" },
      { step: "IN_PROGRESS_STEP_SENTINEL", status: "inProgress" },
      { step: "PENDING_STEP_SENTINEL", status: "pending" },
    ] };
    const snapshot = { thread: nativeThread, connection: "connected", epoch: 1, sequence: 10, settings: {}, pending: [], queue: [] };
    const live = reduceEvent(applySnapshot(freshWorkspace(), snapshot),
      { method: "turn/plan/updated", epoch: 1, sequence: 11, params: plan });
    const restored = applySnapshot(freshWorkspace(), { ...snapshot, sequence: 11, planUpdates: [plan] });
    // These records independently state the Monitor reader contract. They do not
    // call the native adapter or invent a native plan ThreadItem.
    const edit = (id, target, body) => ({ id, turn: 1, src: "", kind: "edit", tool: "fileChange", runtime: { running: false }, head: { name: "File changes", target, chips: [{ x: "completed", c: "state" }] }, body });
    const equivalent = [
      edit("empty-add", "/fixture/empty-added.txt", [{ p: "note", x: "/fixture/empty-added.txt" }, { p: "note", x: "Added file" }, { p: "diff", rows: [["ctx", null, ""]] }]),
      edit("empty-delete", "/fixture/empty-deleted.txt", [{ p: "note", x: "/fixture/empty-deleted.txt" }, { p: "note", x: "Deleted file" }, { p: "diff", rows: [["ctx", null, ""]] }]),
      edit("moved-file", "/fixture/original.txt (moved to /fixture/moved-destination.txt)", [{ p: "note", x: "/fixture/original.txt" }, { p: "note", x: "Moved to: /fixture/moved-destination.txt" }, { p: "diff", rows: [["ctx", null, ""]] }]),
      edit("legacy-change", "", [{ p: "pre", x: legacyOutput }]),
      { id: "opaque-search", turn: 1, src: "", kind: "tool", runtime: { running: false }, head: { name: "Web search", target: "Opaque search query", chips: [{ x: "completed", c: "state" }] }, body: [
        { p: "pre", x: JSON.stringify({ type: "search", query: "Opaque search query" }, null, 2) },
        { p: "pre", x: JSON.stringify([{ title: searchTitle, url: searchUrl, snippet: searchSnippet }], null, 2) },
      ] },
      { id: "preservation-turn:plan-update", turn: 1, src: "", kind: "tool", tool: "TodoWrite", runtime: { running: false }, head: { name: "TodoWrite", chips: [] }, body: [{ p: "pre", x: `Updated Plan\n${explanation}\nCompleted: COMPLETED_STEP_SENTINEL\nInProgress: IN_PROGRESS_STEP_SENTINEL\nPending: PENDING_STEP_SENTINEL` }] },
    ];
    const container = document.createElement("section"); container.style.cssText = "position:absolute;left:-9999px;top:0;width:1000px"; document.body.append(container);
    try {
      const modes = {};
      for (const expanded of [false, true]) {
        const views = [threadRecords(live.thread, undefined, live.planUpdates), threadRecords(restored.thread, undefined, restored.planUpdates), equivalent].map(records => {
          const projection = new Projection(); projection.rebuild(records); projection.rebuild(records, 1);
          const state = { folds: new Map(expanded ? equivalent.map(record => [record.id, false]) : []), processFolds: new Map(), processExpanded: new Set(), processChosen: new Set(), processBulk: new Map(), promptExpanded: new Set(), rawTurns: new Map(), fullTargets: new Set(), headSteps: new Map(), capOpen: new Set(), openImages: new Set(), codeOverrides: new Map() };
          const root = document.createElement("div"); root.append(...projection.units.map(unit => renderUnit(unit, state))); container.append(root);
          return { text: root.innerText, renderers: [...root.querySelectorAll("[data-record-id]")].map(renderer => ({ id: renderer.dataset.recordId, kind: renderer.dataset.rendererKind, closed: renderer.classList.contains("closed"), bodyVisible: !!renderer.querySelector(".renderer-body")?.getClientRects().length, text: renderer.innerText })),
            scripts: root.querySelectorAll("script").length, images: root.querySelectorAll("img").length, activeAttributes: root.querySelectorAll("[onerror],[onload]").length };
        });
        modes[expanded ? "expanded" : "default"] = { live: views[0], snapshot: views[1], reference: views[2] };
      }
      return { ...modes, planUpdates: live.planUpdates, itemIds: live.thread.turns[0].items.map(item => item.id), expectedItemIds: nativeThread.turns[0].items.map(item => item.id), executed: window.preservedFactsExecuted === 1 };
    } finally { container.remove(); }
  });
  assert.deepEqual(result.itemIds, result.expectedItemIds, "Plan notifications do not fabricate native ThreadItems.");
  assert.equal(result.planUpdates.length, 1);
  for (const mode of ["default", "expanded"]) {
    assert.deepEqual(result[mode].live, result[mode].reference, `Live native facts match independent Monitor ${mode} records.`);
    assert.deepEqual(result[mode].snapshot, result[mode].reference, `Restored native facts match independent Monitor ${mode} records.`);
    assert.equal(result[mode].live.renderers.length, 6);
    assert.equal(result[mode].live.scripts, 0); assert.equal(result[mode].live.images, 0); assert.equal(result[mode].live.activeAttributes, 0);
    assert(result[mode].live.renderers.every(renderer => renderer.closed === (mode === "default") && renderer.bodyVisible === (mode === "expanded")));
  }
  const expanded = result.expanded.live;
  const record = id => expanded.renderers.find(renderer => renderer.id === id);
  assert.equal(expanded.renderers.filter(renderer => renderer.id === "preservation-turn:plan-update").length, 1);
  for (const sentinel of ["TodoWrite", "Updated Plan", "PLAN_EXPLANATION_SENTINEL", "Completed: COMPLETED_STEP_SENTINEL", "InProgress: IN_PROGRESS_STEP_SENTINEL", "Pending: PENDING_STEP_SENTINEL"]) assert(record("preservation-turn:plan-update").text.includes(sentinel), `Expanded native plan retains ${sentinel}.`);
  assert.match(record("empty-add").text, /Added file/); assert.match(record("empty-add").text, /empty-added\.txt/);
  assert.match(record("empty-delete").text, /Deleted file/); assert.match(record("empty-delete").text, /empty-deleted\.txt/);
  assert.match(record("moved-file").text, /Moved to: \/fixture\/moved-destination\.txt/);
  assert.match(record("legacy-change").text, /LEGACY_CHANGE_OUTPUT_SENTINEL/);
  for (const sentinel of ["OPAQUE_RESULT_TITLE_SENTINEL", "https://example.invalid/opaque-reference", "OPAQUE_RESULT_SNIPPET_SENTINEL"]) assert(record("opaque-search").text.includes(sentinel), `Opaque web search result retains ${sentinel}.`);
  assert.equal(result.executed, false);
  return { nativePlanUpdatesUseSharedTodoWrite: true, nativePlanLiveAndSnapshotContentPreserved: true,
    emptyFileAddDeleteAndMoveVisible: true, legacyFileChangeOutputVisible: true,
    opaqueWebSearchResultsVisible: true, preservedNativeFactsDefaultExpandedParity: true,
    preservedNativeFactsEscapeActiveContent: true };
}

/** Unknown nested user input stays readable before and after real Rust Markdown rendering. */
async function assertUnknownInputRendering(page) {
  const result = await page.evaluate(async () => {
    const [{ threadRecords, textOfInput }, { Projection }, { renderUnit }, { GatewayApi }] = await Promise.all([
      import("/codex-workspace/model.js"), import("/monitor-ui/view-model.js"),
      import("/monitor-ui/components.js"), import("/codex-workspace/api.js"),
    ]);
    const unknown = { type: "futureAttachment", label: "UNKNOWN_INPUT_SENTINEL", payload: { path: "/fixture/important-resource", markup: '<img src=x onerror="window.unknownInputExecuted=1">' } };
    const content = [{ type: "text", text: "Known prompt control" }, unknown];
    const expected = `Known prompt control\n${JSON.stringify(unknown, null, 2)}`;
    const native = { turns: [{ id: "t", status: "completed", items: [{ id: "u", type: "userMessage", content }] }] };
    const source = threadRecords(native)[0].src;
    const rendered = await new GatewayApi().request("render", { texts: [source] });
    const container = document.createElement("section"); container.style.cssText = "position:absolute;left:-9999px;top:0;width:800px"; document.body.append(container);
    try {
      const variants = [threadRecords(native), threadRecords(native, () => rendered.html[0])].map(records => {
        const projection = new Projection(); projection.rebuild(records);
        const state = { folds: new Map(), processFolds: new Map(), processExpanded: new Set(), processChosen: new Set(), processBulk: new Map(), promptExpanded: new Set(), rawTurns: new Map(), fullTargets: new Set(), headSteps: new Map(), capOpen: new Set(), openImages: new Set(), codeOverrides: new Map() };
        const root = document.createElement("div"); root.append(...projection.units.map(unit => renderUnit(unit, state))); container.append(root);
        return { text: root.innerText, users: root.querySelectorAll(".turn.user").length, scripts: root.querySelectorAll("script").length, images: root.querySelectorAll("img").length, activeAttributes: root.querySelectorAll("[onerror],[onload]").length };
      });
      return { source, expected, queueText: textOfInput(content), variants, executed: window.unknownInputExecuted === 1 };
    } finally { container.remove(); }
  });
  assert.equal(result.source, result.expected); assert.equal(result.queueText, result.expected);
  for (const variant of result.variants) {
    for (const sentinel of ["Known prompt control", "futureAttachment", "UNKNOWN_INPUT_SENTINEL", "/fixture/important-resource", "onerror"]) assert(variant.text.includes(sentinel), `Unknown input retains ${sentinel} through the shared reader.`);
    assert.equal(variant.users, 1); assert.equal(variant.scripts, 0); assert.equal(variant.images, 0); assert.equal(variant.activeAttributes, 0);
  }
  assert.equal(result.executed, false);
  return { unknownInputPayloadVisible: true, unknownInputSafeAfterMarkdownRender: true, unknownQueueInputPayloadRetained: true };
}

/** Read/search/list semantics and unknown-action fallback use the unchanged real reader. */
async function assertCommandExplorationRendering(page) {
  const result = await page.evaluate(async () => {
    const [{ threadRecords }, { Projection }, { renderUnit }] = await Promise.all([
      import("/codex-workspace/model.js"), import("/monitor-ui/view-model.js"), import("/monitor-ui/components.js"),
    ]);
    const command = "cat src/native-sentinel.rs && rg SEARCH_SENTINEL src && rg --files";
    const chips = [{ x: "completed", c: "state" }, { x: "exit 0", c: "state" }, { x: "1.23s", c: "state" }];
    const nativeItem = { id: "exploration", type: "commandExecution", status: "completed", command, cwd: "/fixture", aggregatedOutput: "EXPLORATION_OUTPUT_SENTINEL\n", exitCode: 0, durationMs: 1230, commandActions: [
      { type: "read", command: "cat src/native-sentinel.rs", name: "native-sentinel.rs", path: "/fixture/src/native-sentinel.rs" },
      { type: "search", command: "rg SEARCH_SENTINEL src", query: "SEARCH_SENTINEL", path: "src" },
      { type: "listFiles", command: "rg --files", path: "." },
    ] };
    const negativeItem = { ...nativeItem, id: "mixed-command", command: `${command} && OTHER_WORK_SENTINEL`, commandActions: [...nativeItem.commandActions, { type: "unknown", command: "OTHER_WORK_SENTINEL" }] };
    const record = (id, kind, name, target, body = [], metadata = []) => ({ id, kind, turn: 1, src: "", runtime: { running: false }, ...(kind === "read" || kind === "tool" ? { tool: name } : {}), head: { name, target, chips: metadata }, body });
    const details = [{ p: "pre", x: `$ ${command}` }, { p: "note", x: "Directory: /fixture" }, { p: "pre", x: "EXPLORATION_OUTPUT_SENTINEL\n" }];
    const expected = [
      { ...record("exploration", "act", "Activity", undefined, [{ p: "blocks", items: [
        record("exploration:action:0", "read", "Read", "src/native-sentinel.rs"),
        record("exploration:action:1", "tool", "Grep", "SEARCH_SENTINEL in src"),
        record("exploration:action:2", "tool", "LS", ".", details, chips),
      ] }], chips), head: { name: "Activity", summary: "Searched for 1 pattern, read 1 file, listed 1 directory", chips } },
      { ...record("mixed-command", "bash", "Command", negativeItem.command, details.slice(1), chips), tool: "command" },
    ];
    const native = threadRecords({ cwd: "/fixture", turns: [{ id: "t", status: "completed", items: [nativeItem, negativeItem] }] });
    const container = document.createElement("section"); container.style.cssText = "position:absolute;left:-9999px;top:0;width:800px"; document.body.append(container);
    try {
      const results = {};
      for (const expanded of [false, true]) {
        const views = [native, expected].map(records => {
          const projection = new Projection(); projection.rebuild(records);
          const state = { folds: new Map(expanded ? ["exploration", "exploration:action:0", "exploration:action:1", "exploration:action:2", "mixed-command"].map(id => [id, false]) : []), processFolds: new Map(), processExpanded: new Set(), processChosen: new Set(), processBulk: new Map(), promptExpanded: new Set(), rawTurns: new Map(), fullTargets: new Set(), headSteps: new Map(), capOpen: new Set(), openImages: new Set(), codeOverrides: new Map() };
          const root = document.createElement("div"); root.append(...projection.units.map(unit => renderUnit(unit, state))); container.append(root);
          const exploration = root.querySelector('[data-record-id="exploration"]'), mixed = root.querySelector('[data-record-id="mixed-command"]');
          return { text: root.innerText, activityKind: exploration?.dataset.rendererKind, mixedKind: mixed?.dataset.rendererKind, activityClosed: exploration?.classList.contains("closed"), childCount: exploration?.querySelectorAll(".renderer-children [data-record-id]").length, childrenVisible: !!exploration?.querySelector(".renderer-children")?.getClientRects().length, mixedChildCount: mixed?.querySelectorAll(".renderer-children [data-record-id]").length, scripts: root.querySelectorAll("script").length };
        });
        results[expanded ? "expanded" : "default"] = { native: views[0], reference: views[1] };
      }
      return results;
    } finally { container.remove(); }
  });
  for (const mode of ["default", "expanded"]) {
    assert.deepEqual(result[mode].native, result[mode].reference, `Native exploration matches independent Monitor ${mode} records.`);
    assert.equal(result[mode].native.activityKind, "activity"); assert.equal(result[mode].native.mixedKind, "bash");
    assert.equal(result[mode].native.childCount, 3); assert.equal(result[mode].native.mixedChildCount, 0); assert.equal(result[mode].native.scripts, 0);
    assert.match(result[mode].native.text, /Searched for 1 pattern, read 1 file, listed 1 directory/);
    assert.match(result[mode].native.text, /1\.23s/);
  }
  assert.equal(result.default.native.activityClosed, true); assert.equal(result.default.native.childrenVisible, false);
  assert.equal(result.expanded.native.activityClosed, false); assert.equal(result.expanded.native.childrenVisible, true);
  for (const sentinel of ["Read", "src/native-sentinel.rs", "Grep", "SEARCH_SENTINEL in src", "LS", "EXPLORATION_OUTPUT_SENTINEL", "Directory: /fixture", "OTHER_WORK_SENTINEL", "$ cat src/native-sentinel.rs"]) assert(result.expanded.native.text.includes(sentinel), `Expanded native commands retain ${sentinel}`);
  return { nativeExplorationUsesActivity: true, explorationTargetsCommandOutputPreserved: true, unknownCompoundRemainsBash: true, nativeDurationVisible: true };
}
