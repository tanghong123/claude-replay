import assert from "node:assert/strict";

/** Reopen native settings in the actual browser UI; every Codex API is controlled and loopback-only. */
export async function assertModelSettingsCompatibility(page) {
  const context = await page.context().browser().newContext({ storageState: await page.context().storageState(), serviceWorkers: "block" });
  const fixture = await context.newPage(), errors = [], actions = [], cases = [], checks = {};
  const threadId = "model-settings-fixture";
  const catalogRows = [
    { id: "visible-id", model: "visible-model", displayName: "Visible model", isDefault: true, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low", "high"] },
    { id: "other-id", model: "other-model", displayName: "Other model", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"] },
  ];
  let settings = {}, catalog = catalogRows, epoch = 3, sequence = 10, active = true, rejectCatalog = false, settingsOutcome = "success", settingsHold = null, openHold = null, opens = 0;
  const pending = [{ id: 7, requestToken: "settings-fixture:3:question", canRespond: true, method: "item/tool/requestUserInput", params: { threadId, questions: [{ id: "choice", question: "Choose", options: [{ label: "First" }] }] } }];
  const queue = [{ clientUserMessageId: "queued-fixture", input: [{ type: "text", text: "Queued fixture" }] }];
  const thread = () => ({ id: threadId, cwd: "/fixture", name: "Model settings fixture", turns: active ? [{ id: "active-turn", status: "inProgress", items: [] }] : [] });
  const snapshot = () => ({ thread: thread(), settings: structuredClone(settings), pending, queue, connection: "connected", epoch, sequence });
  const held = () => {
    let release;
    const released = new Promise(resolve => { release = resolve; });
    return { release, released, captured: false };
  };
  fixture.on("pageerror", error => errors.push(error.message));
  await fixture.addInitScript(() => { window.EventSource = class { constructor() { window.settingsFixtureEvents = this; } close() {} }; });
  await context.route("**/api/codex/**", async route => {
    const request = route.request(), url = new URL(request.url()), name = url.pathname.split("/").at(-1);
    let value = { ok: true, data: [], nextCursor: null };
    if (name === "status") value = { ok: true, connection: "connected", epoch, sequence, allowedRoots: ["/fixture"] };
    else if (name === "models") {
      if (rejectCatalog) { await route.fulfill({ status: 503, json: { ok: false, error: { code: "unavailable", message: "CATALOG_UNAVAILABLE_SENTINEL" } } }); return; }
      value.data = catalog;
    } else if (name === "threads") value.data = url.searchParams.get("archived") === "true" ? [] : [thread()];
    else if (name === "open") {
      opens++;
      if (openHold) { const hold = openHold; hold.captured = true; await hold.released; }
      value = { ok: true, snapshot: snapshot() };
    } else if (name === "settings") {
      const payload = request.postDataJSON(); actions.push({ route: name, payload });
      const hold = settingsHold;
      const model = catalogRows.find(row => row.id === payload.model || row.model === payload.model)?.model;
      const accepted = { ...snapshot(), settings: { model, effort: payload.effort }, sequence: sequence + 1 };
      if (hold) { hold.captured = true; await hold.released; }
      if (settingsOutcome === "failure") { await route.fulfill({ status: 409, json: { ok: false, error: { code: "conflict", message: "SETTINGS_REJECTED_SENTINEL" } } }); return; }
      if (settingsOutcome === "uncertain") {
        settings = accepted.settings; sequence++;
        await route.fulfill({ status: 503, json: { ok: false, error: { code: "uncertain", message: "SETTINGS_UNCERTAIN_SENTINEL" } } }); return;
      }
      if (settingsOutcome === "stale") value = { ok: true, snapshot: accepted };
      else if (settingsOutcome === "empty") value = { ok: true };
      else {
        settings = accepted.settings; sequence = Math.max(sequence + 1, accepted.sequence);
        value = { ok: true, snapshot: snapshot() };
      }
    } else if (["send", "interrupt", "cancel", "respond"].includes(name)) {
      actions.push({ route: name, payload: request.postDataJSON() });
      value = { ok: true, delivered: true };
    } else if (name === "render") value = { ok: true, html: request.postDataJSON().texts.map(() => "") };
    else assert.equal(name, "projects", `Only controlled settings APIs are expected, received ${name}`);
    await route.fulfill({ json: value });
  });
  const renderBarrier = () => fixture.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const open = async (value, available = catalogRows) => {
    settings = value; catalog = available; sequence = 10; rejectCatalog = false; settingsOutcome = "success";
    await fixture.goto(`${new URL(page.url()).origin}/codex-workspace?thread=${threadId}`);
    await fixture.waitForFunction(() => document.getElementById("thread-title").textContent === "Model settings fixture" && document.getElementById("connection-status").textContent === "Connected to execution computer" && !document.getElementById("interrupt-turn").disabled);
    await renderBarrier();
  };
  const observe = () => fixture.evaluate(() => {
    const model = document.getElementById("model-select"), effort = document.getElementById("effort-select");
    return { model: model.value, modelLabel: model.selectedOptions[0]?.textContent, effort: effort.value, effortLabel: effort.selectedOptions[0]?.textContent, note: document.getElementById("settings-note").textContent, sendDisabled: document.getElementById("send-message").disabled, steerDisabled: document.getElementById("steer-message").disabled, stopDisabled: document.getElementById("interrupt-turn").disabled, questionDisabled: document.querySelector('[data-testid="submit-answer"]').disabled, cancelDisabled: document.querySelector("[data-cancel-queue]").disabled, draft: document.getElementById("composer-input").value };
  });
  const bypassSendControls = async () => {
    const before = actions.length;
    await fixture.evaluate(() => {
      const input = document.getElementById("composer-input"); input.value = "Keep unsupported-settings draft"; input.oninput();
      const form = document.getElementById("composer-form"); form.onsubmit({ preventDefault() {} });
      input.onkeydown({ key: "Enter", shiftKey: false, ctrlKey: true, preventDefault() {} });
      document.getElementById("steer-message").onclick();
    });
    await renderBarrier();
    assert.equal(actions.length, before, "Programmatic submit, keyboard send and steer cannot bypass unavailable effective settings.");
    assert.equal(await fixture.locator("#send-message").isDisabled(), true, "Input changes cannot silently enable default-model sends.");
  };
  const emitSettings = async value => {
    settings = value;
    await fixture.evaluate(event => window.settingsFixtureEvents.onmessage({ data: JSON.stringify(event) }), { method: "thread/settings/updated", params: { threadId, threadSettings: value }, epoch, sequence: ++sequence });
    await renderBarrier();
  };
  const awaitAction = async count => {
    const deadline = Date.now() + 10000;
    while (actions.length < count && Date.now() < deadline) await fixture.waitForTimeout(20);
    assert.equal(actions.length, count);
  };
  const waitObserved = async (condition, message) => {
    const deadline = Date.now() + 10000;
    while (!condition() && Date.now() < deadline) await fixture.waitForTimeout(20);
    assert(condition(), message);
  };
  const settleControlRead = async previousOpens => {
    await waitObserved(() => opens > previousOpens, "The explicit native control is followed by its ordinary snapshot refresh.");
    await fixture.waitForFunction(() => !document.getElementById("interrupt-turn").disabled);
    await renderBarrier();
  };
  try {
    const unsupported = [
      ["absent model", { model: "absent-model", effort: "high" }],
      ["hidden model omitted from catalog", { model: "hidden-model", effort: "high" }],
      ["obsolete effort", { model: "visible-model", effort: "obsolete-effort" }],
      ["both unavailable", { model: "absent-model", effort: "obsolete-effort" }],
      ["missing settings", {}],
      ["missing model", { effort: "high" }],
      ["missing effort", { model: "visible-model" }],
    ];
    for (const [name, value] of unsupported) {
      await open(value); await bypassSendControls();
      const actual = await observe();
      assert.equal(actual.model, value.model === "visible-model" ? "visible-id" : value.model || "");
      assert.equal(actual.effort, value.effort || "");
      assert.match(actual.note, /unavailable|not confirmed/);
      assert.equal(actual.sendDisabled, true); assert.equal(actual.steerDisabled, true);
      assert.equal(actual.stopDisabled, false); assert.equal(actual.questionDisabled, false); assert.equal(actual.cancelDisabled, false);
      assert.equal(actual.draft, "Keep unsupported-settings draft");
      cases.push({ name, nativeSettings: value, actual, writerCount: actions.length });
    }
    checks.unavailableSettingsRemainVisibleAndCannotSend = true;

    // Unsupported settings affect new messages, never existing native controls.
    const controlStart = actions.length;
    const stopOpens = opens;
    await fixture.locator("#interrupt-turn").click(); await awaitAction(controlStart + 1);
    await settleControlRead(stopOpens);
    const questionOpens = opens;
    await fixture.locator('input[type="radio"][data-question-id="choice"]').check();
    await fixture.locator('[data-testid="submit-answer"]').click(); await awaitAction(controlStart + 2);
    await settleControlRead(questionOpens);
    await fixture.locator("[data-cancel-queue]").click(); await awaitAction(controlStart + 3);
    assert.deepEqual(actions.slice(controlStart).map(value => value.route), ["interrupt", "respond", "cancel"]);
    checks.unavailableSettingsKeepStopQuestionsAndQueueCancel = true;

    // Creating a new task remains an intentional choice of advertised defaults.
    await fixture.locator("#new-thread").click();
    await fixture.locator("#new-task-dialog").waitFor({ state: "visible" });
    assert.equal(await fixture.locator("#new-model-select").inputValue(), "visible-id");
    assert.equal(await fixture.locator("#new-effort-select").inputValue(), "low");
    await fixture.locator("#close-new-task").click();
    checks.newTaskAdvertisedDefaultsRemainAvailable = true;

    for (const model of ["visible-model", "visible-id"]) {
      await open({ model, reasoningEffort: "high" });
      await fixture.locator("#composer-input").fill("Alias send fixture");
      assert.equal(await fixture.locator("#send-message").isDisabled(), false);
      assert.equal(await fixture.locator("#model-select").inputValue(), "visible-id");
      assert.equal(await fixture.locator("#effort-select").inputValue(), "high");
      const before = actions.length;
      await fixture.evaluate(() => document.getElementById("composer-form").onsubmit({ preventDefault() {} }));
      await awaitAction(before + 1); assert.equal(actions.at(-1).route, "send");
      assert.equal(actions.slice(before).some(value => value.route === "settings"), false, "Known effective aliases require no hidden settings action.");
      cases.push({ name: `supported alias ${model}`, nativeSettings: { model, reasoningEffort: "high" }, actual: await observe() });
    }
    checks.nativeModelAliasesAndEffortSpellingEnableTruthfulSends = true;

    await open({ model: "visible-model", effort: "high" }, []); await bypassSendControls();
    assert.equal(await fixture.locator("#model-select").inputValue(), "visible-model");
    assert.equal(await fixture.locator("#effort-select").inputValue(), "high");
    assert.equal(await fixture.locator("#model-select").isDisabled(), true);
    assert.equal(await fixture.locator("#interrupt-turn").isDisabled(), false);
    cases.push({ name: "empty catalog", actual: await observe() });

    await open({ model: "absent-model", effort: "obsolete-effort" });
    await fixture.locator("#composer-input").fill("Preserve selected-model draft");
    const successStart = actions.length;
    settingsHold = held();
    await fixture.locator("#model-select").selectOption("other-id");
    await awaitAction(successStart + 1);
    assert.equal(settingsHold.captured, true);
    assert.deepEqual({ model: actions.at(-1).payload.model, effort: actions.at(-1).payload.effort }, { model: "other-id", effort: "medium" });
    assert.equal(await fixture.locator("#send-message").isDisabled(), true, "A pending settings action cannot enable the selected defaults.");
    assert.equal(await fixture.locator("#model-select").inputValue(), "absent-model", "The picker still displays native effective settings while confirmation is pending.");
    settingsHold.release(); settingsHold = null;
    await fixture.waitForFunction(() => !document.getElementById("send-message").disabled && document.getElementById("model-select").value === "other-id");
    assert.equal(await fixture.locator("#effort-select").inputValue(), "medium");
    assert.equal(await fixture.locator("#composer-input").inputValue(), "Preserve selected-model draft");
    cases.push({ name: "explicit confirmed model recovery", action: actions.at(-1), actual: await observe() });

    await open({ model: "visible-model", effort: "obsolete-effort" });
    await fixture.locator("#composer-input").fill("Preserve failed-settings draft");
    settingsOutcome = "failure";
    await fixture.locator("#effort-select").selectOption("high");
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent === "SETTINGS_REJECTED_SENTINEL");
    assert.equal(await fixture.locator("#effort-select").inputValue(), "obsolete-effort");
    assert.equal(await fixture.locator("#send-message").isDisabled(), true);
    assert.equal(await fixture.locator("#composer-input").inputValue(), "Preserve failed-settings draft");
    settingsOutcome = "success";
    await fixture.locator("#effort-select").selectOption("high");
    await fixture.waitForFunction(() => !document.getElementById("send-message").disabled && document.getElementById("workspace-error").hidden);
    cases.push({ name: "explicit effort failure and recovery", actual: await observe() });
    checks.explicitSettingsRequireNativeConfirmationAndPreserveDrafts = true;

    // A newer native settings event wins over the older action snapshot.
    await open({ model: "absent-model", effort: "high" });
    await fixture.locator("#composer-input").fill("Preserve stale-settings draft");
    settingsOutcome = "stale"; settingsHold = held(); openHold = held();
    await fixture.locator("#model-select").selectOption("visible-id");
    await waitObserved(() => settingsHold.captured, "The explicit settings request reached its held response.");
    await emitSettings({ model: "absent-model", effort: "obsolete-effort" });
    await emitSettings({ model: "newer-hidden-model", effort: "obsolete-effort" });
    settingsHold.release(); settingsHold = null;
    await fixture.waitForFunction(() => !document.getElementById("model-select").disabled);
    await renderBarrier();
    assert.match(await fixture.locator("#workspace-error").innerText(), /has not confirmed/, "A stale successful response cannot claim that the requested settings are effective.");
    assert.equal(await fixture.locator("#model-select").inputValue(), "newer-hidden-model");
    assert.equal(await fixture.locator("#send-message").isDisabled(), true);
    assert.equal(await fixture.locator("#composer-input").inputValue(), "Preserve stale-settings draft");
    settings = { model: "visible-model", effort: "high" }; sequence++;
    openHold.release(); openHold = null;
    await fixture.waitForFunction(() => !document.getElementById("send-message").disabled);
    cases.push({ name: "stale settings response fenced until native reread", actual: await observe() });
    checks.staleSettingsCannotReplaceNewerNativeSettings = true;

    await open({ model: "absent-model", effort: "high" });
    await fixture.locator("#composer-input").fill("Preserve empty-ack draft");
    settingsOutcome = "empty";
    await fixture.locator("#model-select").selectOption("visible-id");
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent.includes("did not confirm"));
    assert.equal(await fixture.locator("#send-message").isDisabled(), true);
    cases.push({ name: "empty settings acknowledgement is insufficient", actual: await observe() });

    await open({ model: "absent-model", effort: "high" });
    await fixture.locator("#composer-input").fill("Preserve uncertain-settings draft");
    settingsOutcome = "uncertain"; openHold = held();
    await fixture.locator("#model-select").selectOption("visible-id");
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent === "SETTINGS_UNCERTAIN_SENTINEL");
    assert.equal(await fixture.locator("#send-message").isDisabled(), true);
    assert.equal(await fixture.locator("#composer-input").inputValue(), "Preserve uncertain-settings draft");
    openHold.release(); openHold = null;
    await fixture.waitForFunction(() => !document.getElementById("send-message").disabled);
    assert.equal(await fixture.locator("#model-select").inputValue(), "visible-id");
    assert.equal(await fixture.locator("#workspace-error").innerText(), "SETTINGS_UNCERTAIN_SENTINEL", "Recovery preserves the uncertain action's observable result.");
    cases.push({ name: "uncertain settings wait for native reread", actual: await observe() });
    checks.settingsAcknowledgementsAndUncertainUpdatesRequireFreshProof = true;

    await open({ model: "visible-model", effort: "high" });
    await fixture.locator("#composer-input").fill("Preserve status-epoch draft");
    for (const replacement of [epoch + 1, 1]) {
      const previousOpens = opens;
      epoch = replacement; sequence = 1;
      await fixture.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
      await waitObserved(() => opens > previousOpens, "A status-only native epoch replacement must reach a fresh task snapshot after refreshing its catalog.");
      await fixture.waitForFunction(() => !document.getElementById("interrupt-turn").disabled);
      assert.equal(await fixture.locator("#send-message").isDisabled(), false, "Catalog refresh binds the new status epoch before the replacement task snapshot, including a restarted Gateway's reset epoch.");
      cases.push({ name: `status-only native epoch replacement ${replacement}`, actual: await observe() });
    }
    checks.statusOnlyEpochReplacementRefreshesCapabilities = true;

    await open({ model: "visible-model", effort: "high" });
    await fixture.locator("#composer-input").fill("Preserve refreshed-catalog draft");
    settingsHold = held();
    await fixture.locator("#model-select").selectOption("other-id");
    await waitObserved(() => settingsHold.captured, "The catalog-race settings request reached its held response.");
    rejectCatalog = true;
    await fixture.evaluate(event => window.settingsFixtureEvents.onmessage({ data: JSON.stringify(event) }), { method: "gateway/connection", params: { state: "reconnecting" }, epoch, sequence: ++sequence });
    await fixture.evaluate(event => window.settingsFixtureEvents.onmessage({ data: JSON.stringify(event) }), { method: "gateway/connection", params: { state: "connected" }, epoch, sequence: ++sequence });
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent === "CATALOG_UNAVAILABLE_SENTINEL");
    // A confirmed action snapshot can arrive after catalog recovery failed. It
    // restores native control proofs, but cannot restore stale model inventory.
    settingsHold.release(); settingsHold = null;
    await fixture.waitForFunction(() => !document.getElementById("interrupt-turn").disabled);
    await fixture.evaluate(() => document.getElementById("composer-input").oninput());
    assert.equal(await fixture.locator("#send-message").isDisabled(), true, "A failed catalog refresh cannot reuse old model availability to send.");
    assert.equal(await fixture.locator("#model-select").isDisabled(), true);
    assert.equal(await fixture.locator("#interrupt-turn").isDisabled(), false, "Confirmed native controls remain usable even without a fresh model catalog.");
    rejectCatalog = false; catalog = catalogRows.slice(0, 1);
    await fixture.waitForFunction(() => !document.getElementById("model-select").disabled);
    assert.equal(await fixture.locator("#model-select").inputValue(), "other-model");
    assert.equal(await fixture.locator("#send-message").isDisabled(), true, "A refreshed catalog that no longer advertises the saved model still requires explicit selection.");
    assert.equal(await fixture.locator("#composer-input").inputValue(), "Preserve refreshed-catalog draft");
    cases.push({ name: "catalog failure and changed inventory recovery", actual: await observe() });
    checks.unavailableAndRefreshedCatalogsCannotReuseDefaults = true;

    const sourceSha256 = await fixture.evaluate(async () => Object.fromEntries(await Promise.all(["app.js", "model.js"].map(async name => {
      const response = await fetch(`/codex-workspace/${name}`, { cache: "no-store" });
      if (!response.ok) throw new Error(`Cannot identify settings UI source ${name}: ${response.status}`);
      const digest = await crypto.subtle.digest("SHA-256", await response.arrayBuffer());
      return [name, [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("")];
    }))));
    assert.deepEqual(errors, []);
    return { checks, cases, sourceSha256, actions, openCount: opens };
  } finally {
    settingsHold?.release(); openHold?.release();
    await context.close();
  }
}
