import assert from "node:assert/strict";

/** Exercise selected-task archive actions in the actual UI; all native APIs are mocked. */
export async function assertSelectedThreadArchiveControls(page) {
  const context = await page.context().browser().newContext({ storageState: await page.context().storageState(), serviceWorkers: "block" });
  const fixture = await context.newPage();
  const errors = [], actions = [], writerRequests = [], opens = [], inventoryResponses = [], rows = new Map([["active-task", false], ["archived-task", true]]);
  let epoch = 1, sequence = 10, connection = "connected", rejectArchive = false, heldAction = null, heldInventories = null, heldOpen = null, rejectOpen = false;
  const thread = id => ({ id, cwd: "/fixture", name: id, turns: [] });
  fixture.on("pageerror", error => errors.push(error.message));
  await fixture.addInitScript(() => {
    window.EventSource = class { constructor() { window.archiveControlEvents = this; } close() {} };
    window.archiveInventoryReads = [];
    const fetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const url = new URL(args[0], location.href);
      if (url.pathname !== "/api/codex/threads") return fetch(...args);
      const stack = new Error().stack;
      const read = { url: url.pathname + url.search, reader: stack.includes("loadSelectedArchive") ? "selected" : stack.includes("loadHistory") ? "sidebar" : "other", parsed: false };
      window.archiveInventoryReads.push(read);
      const response = await fetch(...args);
      const json = response.json.bind(response);
      response.json = async () => { const value = await json(); read.parsed = true; return value; };
      return response;
    };
  });
  await context.route("**/api/codex/**", async route => {
    const request = route.request(), url = new URL(request.url()), name = url.pathname.split("/").at(-1);
    let value = { ok: true, data: [], nextCursor: null }, inventoryResponse = null;
    if (name === "status") value = { ok: true, connection, epoch, sequence, allowedRoots: ["/fixture"] };
    else if (name === "models") value.data = [{ id: "fixture-model", isDefault: true, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low"] }];
    else if (name === "threads") {
      const archived = url.searchParams.get("archived") === "true";
      value.data = [...rows].filter(([, state]) => state === archived).map(([id]) => thread(id));
      inventoryResponse = { archived, ids: value.data.map(row => row.id), epoch, sequence, held: null, fulfilled: false };
      inventoryResponses.push(inventoryResponse);
      const inventoryHold = heldInventories;
      if (inventoryHold && inventoryHold.epoch === epoch && inventoryHold.sequence === sequence) {
        inventoryResponse.held = inventoryHold.name;
        inventoryHold.captured++;
        await inventoryHold.released;
      }
    } else if (name === "open") {
      const { threadId } = request.postDataJSON();
      const accessMode = rows.get(threadId) ? "archived" : "active";
      opens.push({ threadId, accessMode });
      if (heldOpen) { heldOpen.captured = true; await heldOpen.released; }
      if (rejectOpen) {
        await route.fulfill({ status: 503, json: { ok: false, error: { code: "unavailable", message: "ATTACHMENT_REJECTED_SENTINEL" } } }); return;
      }
      value = { ok: true, snapshot: { thread: thread(threadId), accessMode, settings: accessMode === "archived" ? {} : { model: "fixture-model", effort: "low" }, pending: [], queue: [], connection: "connected", epoch, sequence } };
    } else if (name === "archive") {
      const payload = request.postDataJSON();
      actions.push(payload);
      if (rejectArchive) {
        rejectArchive = false;
        await route.fulfill({ status: 409, json: { ok: false, error: { code: "conflict", message: "ARCHIVE_REJECTED_SENTINEL" } } });
        return;
      }
      rows.set(payload.threadId, payload.archived);
      if (heldAction) { heldAction.captured = true; await heldAction.released; }
    } else if (["send", "settings", "interrupt", "cancel", "respond"].includes(name)) {
      writerRequests.push(name); await route.fulfill({ status: 409, json: { ok: false, error: { code: "conflict", message: "An archived fixture must not write." } } }); return;
    } else assert.equal(name, "projects", `Only mocked archive actions and workspace reads are expected, received ${name}`);
    await route.fulfill({ json: value });
    if (inventoryResponse) inventoryResponse.fulfilled = true;
  });
  const label = () => fixture.locator("#archive-thread").getAttribute("aria-label");
  const expectLabel = async expected => {
    await fixture.waitForFunction(value => document.getElementById("archive-thread").getAttribute("aria-label") === value && !document.getElementById("archive-thread").disabled, expected);
    assert.equal(await label(), expected);
  };
  const open = async id => {
    await fixture.goto(`${new URL(page.url()).origin}/codex-workspace?thread=${id}`);
    await fixture.waitForFunction(value => document.getElementById("thread-title").textContent === value && document.getElementById("connection-status").textContent === "Connected to execution computer", id);
  };
  const waitAction = async count => {
    const deadline = Date.now() + 10000;
    while (actions.length < count && Date.now() < deadline) await fixture.waitForTimeout(20);
    assert.equal(actions.length, count);
  };
  const emit = async (method, params = {}) => {
    sequence++;
    if (method === "gateway/connection") connection = params.state;
    await fixture.evaluate(event => window.archiveControlEvents.onmessage({ data: JSON.stringify(event) }), { method, params, epoch, sequence });
  };
  const disconnect = async () => {
    // A background recovery must observe the same outage until an explicit
    // connected event, rather than being healed by an always-connected mock.
    connection = "reconnecting";
    await fixture.evaluate(() => window.archiveControlEvents.onerror());
  };
  const hold = () => {
    let release;
    const released = new Promise(resolve => { release = resolve; });
    return { release, released, captured: false };
  };
  const drainInventories = async () => {
    await fixture.waitForFunction(() => window.archiveInventoryReads.every(read => read.parsed));
    // JSON consumption runs before the application's awaiting continuation.
    // A rendered frame proves those microtasks finished before the next case.
    await fixture.evaluate(() => new Promise(resolve => requestAnimationFrame(resolve)));
    assert(inventoryResponses.every(response => response.fulfilled), "Every previous mock inventory response is drained.");
  };
  const holdInventory = (name, nextEvent = false) => {
    const inventoryHold = { ...hold(), name, epoch, sequence: sequence + Number(nextEvent), captured: 0 };
    heldInventories = inventoryHold;
    return inventoryHold;
  };
  const expectHeldInventory = async (inventoryHold, view, member) => {
    const deadline = Date.now() + 10000;
    while (inventoryHold.captured === 0 && Date.now() < deadline) await fixture.waitForTimeout(20);
    const held = inventoryResponses.filter(response => response.held === inventoryHold.name);
    assert(held.some(response => response.archived === view && response.ids.includes("active-task") === member
      && !response.fulfilled), `${inventoryHold.name} has an actual unfulfilled response with the old selected-task membership.`);
  };
  const releaseInventory = async inventoryHold => {
    if (heldInventories === inventoryHold) heldInventories = null;
    inventoryHold.release();
    await drainInventories();
    assert(inventoryResponses.filter(response => response.held === inventoryHold.name).every(response => response.fulfilled),
      `${inventoryHold.name} responses are fulfilled and consumed before checking state.`);
  };
  const assertArchivedWriters = async () => {
    await fixture.waitForFunction(() => document.getElementById("runtime-status").textContent === "Archived task — restore to continue");
    for (const id of ["composer-input", "send-message", "steer-message", "model-select", "effort-select", "interrupt-turn"]) assert.equal(await fixture.locator(`#${id}`).isDisabled(), true, `${id} is disabled for archived history`);
    assert.equal(await fixture.locator("#model-select").inputValue(), "");
    assert.equal(await fixture.locator("#effort-select").inputValue(), "");
    assert.equal(await fixture.locator("#model-select").innerText(), "Historical model unavailable");
    assert.equal(await fixture.locator("#effort-select").innerText(), "Historical effort unavailable");
    await fixture.evaluate(() => {
      const input = document.getElementById("composer-input"); input.value = "ARCHIVED_WRITE_SENTINEL";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      document.getElementById("composer-form").dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
      for (const id of ["model-select", "effort-select"]) document.getElementById(id).dispatchEvent(new Event("change", { bubbles: true }));
      document.getElementById("steer-message").onclick(); document.getElementById("interrupt-turn").onclick();
      const queue = document.getElementById("queued-messages"), button = document.createElement("button"); button.dataset.cancelQueue = "stale-queue"; queue.append(button);
      queue.onclick({ target: button }); button.remove();
    });
    await fixture.waitForTimeout(50);
    assert.deepEqual(writerRequests, []); assert.equal(await fixture.locator("#send-message").isDisabled(), true);
  };
  try {
    await open("active-task"); await expectLabel("Archive this task");
    await fixture.locator("#show-archived").click();
    await fixture.locator('[data-thread-id="archived-task"]').waitFor();
    await expectLabel("Archive this task");
    await fixture.locator("#archive-thread").click(); await waitAction(1);
    assert.equal(actions[0].threadId, "active-task"); assert.equal(actions[0].archived, true);
    await expectLabel("Restore this task");
    assert.equal(await fixture.locator("#show-archived").getAttribute("aria-pressed"), "true");
    await fixture.locator("#show-active").click();
    await fixture.waitForFunction(() => document.getElementById("show-active").getAttribute("aria-pressed") === "true");
    await expectLabel("Restore this task");
    await fixture.locator("#archive-thread").click(); await waitAction(2);
    assert.equal(actions[1].threadId, "active-task"); assert.equal(actions[1].archived, false);
    await expectLabel("Archive this task");

    await fixture.locator("#show-archived").click();
    await fixture.locator('[data-thread-id="archived-task"]').waitFor();
    rejectArchive = true;
    await fixture.locator("#archive-thread").click(); await waitAction(3);
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent === "ARCHIVE_REJECTED_SENTINEL");
    assert.equal(actions[2].archived, true); assert.equal(rows.get("active-task"), false);
    await expectLabel("Archive this task");
    assert.equal(await fixture.locator("#show-archived").getAttribute("aria-pressed"), "true");

    rows.set("active-task", true); await emit("thread/archived", { threadId: "active-task" });
    await expectLabel("Restore this task");
    await fixture.locator("#show-active").click(); await expectLabel("Restore this task");
    rows.set("active-task", false); await emit("thread/unarchived", { threadId: "active-task" });
    await expectLabel("Archive this task");

    // A same-epoch reconnect must repair a notification missed while disconnected.
    rows.set("active-task", true);
    await disconnect();
    await fixture.waitForFunction(() => document.getElementById("archive-thread").disabled);
    assert.equal(await fixture.locator("#archive-thread").isDisabled(), true);
    connection = "connected";
    await fixture.evaluate(event => window.archiveControlEvents.onmessage({ data: JSON.stringify(event) }), { connection, epoch, sequence });
    await expectLabel("Restore this task");

    // Older inventory reads must not replace a later ordered native notification.
    rows.set("active-task", false); await emit("thread/unarchived", { threadId: "active-task" });
    await expectLabel("Archive this task");
    await drainInventories();
    const inventoryHold = holdInventory("older-than-native-notification", true);
    await emit("gateway/historyChanged");
    await expectHeldInventory(inventoryHold, false, true);
    rows.set("active-task", true); await emit("thread/archived", { threadId: "active-task" });
    await expectLabel("Restore this task");
    await releaseInventory(inventoryHold); await expectLabel("Restore this task");

    // Hold a row that actually classifies this task as archived. An empty old
    // active inventory cannot discriminate an incorrect new-epoch overwrite.
    await fixture.locator("#show-archived").click(); await expectLabel("Restore this task");
    await drainInventories();
    const epochHold = holdInventory("older-server-epoch", true);
    await emit("gateway/historyChanged");
    await expectHeldInventory(epochHold, true, true);
    await disconnect();
    epoch++; sequence = 20; rows.set("active-task", false);
    connection = "connected";
    await fixture.evaluate(event => window.archiveControlEvents.onmessage({ data: JSON.stringify(event) }), { connection, epoch, sequence });
    await expectLabel("Archive this task");
    await releaseInventory(epochHold); await expectLabel("Archive this task");

    // Cache invalidation must also fence an older sidebar read whose task entry was cleared.
    await drainInventories();
    const historyHold = holdInventory("invalidated-sidebar-inventory");
    await fixture.locator("#show-active").click();
    await expectHeldInventory(historyHold, false, true);
    rows.set("active-task", true);
    await disconnect();
    await releaseInventory(historyHold);
    assert.equal(await label(), "Checking task archive status", "An invalidated older sidebar row cannot relabel the selected task.");
    await emit("gateway/connection", { state: "connected" });
    await expectLabel("Restore this task");

    await open("archived-task"); await expectLabel("Restore this task");
    await fixture.locator("#show-active").click(); await expectLabel("Restore this task");
    await open("unknown-task");
    assert.equal(await fixture.locator("#archive-thread").isDisabled(), true);
    assert.equal(await label(), "Checking task archive status");
    assert.equal(actions.length, 3, "Unknown native inventory membership must not generate an action.");

    // Capture the action target before awaiting its result; a new selection keeps its state.
    rows.set("active-task", false);
    await open("active-task"); await expectLabel("Archive this task");
    await fixture.locator("#show-archived").click();
    await fixture.locator('[data-thread-id="archived-task"]').waitFor();
    const actionHold = hold(); heldAction = actionHold;
    await fixture.locator("#archive-thread").click(); await waitAction(4);
    assert.equal(actions[3].threadId, "active-task"); assert.equal(actions[3].archived, true);
    await emit("thread/archived", { threadId: "active-task" });
    await fixture.locator('[data-thread-id="archived-task"]').click();
    await fixture.waitForFunction(() => document.getElementById("thread-title").textContent === "archived-task");
    actionHold.release(); heldAction = null;
    await expectLabel("Restore this task");
    assert.equal(new URL(fixture.url()).searchParams.get("thread"), "archived-task");
    assert.equal(await fixture.locator("#show-archived").getAttribute("aria-pressed"), "true");

    // Cold archived history has no known effective model/effort, and cannot write.
    await open("archived-task"); await expectLabel("Restore this task"); await assertArchivedWriters();
    const openHold = hold(); heldOpen = openHold;
    const beforeRestoreOpens = opens.length;
    await fixture.locator("#archive-thread").click(); await waitAction(5);
    const openDeadline = Date.now() + 10000;
    while (!openHold.captured && Date.now() < openDeadline) await fixture.waitForTimeout(20);
    assert.equal(openHold.captured, true);
    assert.deepEqual(opens[beforeRestoreOpens], { threadId: "archived-task", accessMode: "active" });
    for (const id of ["composer-input", "send-message", "model-select", "effort-select"]) assert.equal(await fixture.locator(`#${id}`).isDisabled(), true, "A held reattachment does not enable writers");
    openHold.release(); heldOpen = null;
    await fixture.waitForFunction(() => !document.getElementById("composer-input").disabled && !document.getElementById("send-message").disabled && document.getElementById("model-select").value === "fixture-model");

    rows.set("archived-task", true); await open("archived-task"); await expectLabel("Restore this task"); await assertArchivedWriters();
    rejectOpen = true;
    await fixture.locator("#archive-thread").click(); await waitAction(6);
    await fixture.waitForFunction(() => document.getElementById("workspace-error").textContent === "ATTACHMENT_REJECTED_SENTINEL");
    for (const id of ["composer-input", "send-message", "model-select", "effort-select"]) assert.equal(await fixture.locator(`#${id}`).isDisabled(), true, "A failed reattachment does not enable writers");
    rejectOpen = false;
    await fixture.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await fixture.waitForFunction(() => !document.getElementById("composer-input").disabled && !document.getElementById("model-select").disabled);
    assert.deepEqual(errors, []);
    return { selectedActiveArchivesInArchivedView: true, selectedArchiveRestoresInActiveView: true,
      failedArchiveKeepsState: true, nativeArchiveNotificationsAuthoritative: true,
      sameEpochReconnectRepairsArchiveState: true, staleInventoryCannotReplaceNativeArchiveEvent: true,
      staleInventoryCannotReplaceNewEpoch: true,
      invalidatedHistoryReadCannotReclassifySelection: true,
      directUrlArchivedState: true, unknownArchiveStateDisablesAction: true,
      notificationBeforeActionResponseKeepsDesiredState: true, pendingArchiveDoesNotChangeNewSelection: true,
      archivedSnapshotDisablesWriters: true, unknownHistoricalSettingsAreNotCatalogDefaults: true,
      programmaticArchivedInputCannotWrite: true, restoreRequiresFreshNativeOpen: true,
      heldRestoreAttachmentCannotWrite: true, failedRestoreAttachmentCannotWrite: true,
      successfulRestoreAttachmentEnablesWriters: true };
  } catch (error) {
    let observed;
    try { observed = await fixture.evaluate(() => ({ title: document.getElementById("thread-title")?.textContent, connection: document.getElementById("connection-status")?.textContent, error: document.getElementById("workspace-error")?.textContent, archive: document.getElementById("archive-thread")?.getAttribute("aria-label"), inventoryReads: window.archiveInventoryReads })); }
    catch (cause) { observed = { unavailable: cause.message }; }
    throw new Error(`${error.message}\nArchive control observations: ${JSON.stringify({ observed, inventoryResponses, errors, actions: actions.map(({ threadId, archived }) => ({ threadId, archived })) })}`, { cause: error });
  } finally {
    heldAction?.release(); heldInventories?.release(); heldOpen?.release(); await context.close();
  }
}
