import assert from "node:assert/strict";
import { test } from "node:test";
import { RecordStore } from "../src/codex-ui/record-store.js";

/** New native sessions exist before a JSONL file; other unreadable histories remain errors. */
test("native-confirmed empty history opens without treating a missing rollout as an error", async () => {
  const original = globalThis.fetch;
  const calls = [];
  const store = new RecordStore({ empty: () => calls.push("empty"), opened: () => calls.push("opened"), error: error => calls.push(error.message) }, { emptySession: id => id === "new-native-rollout" });
  store.session = "new-native-rollout"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    await store.poll(1); clearTimeout(store.timer);
    await store.poll(1);
    assert.deepEqual(calls, ["empty", "opened", "empty"]);
    assert.equal(store.records.length, 0);
    assert.equal(store.shown, true);
  } finally { store.stop(); globalThis.fetch = original; }
});

test("an ordinary missing or unauthorized history never receives the native empty-session fallback", async () => {
  const original = globalThis.fetch;
  try {
    for (const [status, emptySession] of [[404, () => false], [403, () => true], [500, () => true]]) {
      const calls = [], store = new RecordStore({ empty: () => calls.push("empty"), error: error => calls.push(error.message) }, { emptySession });
      store.session = "existing-rollout"; store.generation = 1;
      globalThis.fetch = async () => ({ ok: false, status });
      await store.poll(1); store.stop();
      assert.deepEqual(calls, [`pull HTTP ${status}`]); assert.equal(store.shown, false);
    }
  } finally { globalThis.fetch = original; }
});

test("a confirmed native empty session becomes readable while the first JSONL pull is still pending", async () => {
  const original = globalThis.fetch, calls = []; let confirmed = false, release;
  const store = new RecordStore({ empty: () => calls.push("empty"), opened: () => calls.push("opened"), error: error => calls.push(error.message) }, { emptySession: () => confirmed });
  store.session = "new-rollout"; store.generation = 1;
  globalThis.fetch = () => new Promise(resolve => { release = resolve; });
  try {
    const pending = store.poll(1);
    assert.equal(store.refreshEmpty(), false);
    confirmed = true;
    assert.equal(store.refreshEmpty(), true);
    assert.deepEqual(calls, ["empty", "opened"]);
    release({ ok: false, status: 404 }); await pending;
    assert.equal(calls.filter(call => call === "opened").length, 1);
  } finally { store.stop(); globalThis.fetch = original; }
});


test("a definite native rejection replaces waiting even while the first pull is pending", async t => {
  const original = globalThis.fetch, calls = []; let failure = null, release;
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const store = new RecordStore({ waiting: () => calls.push("waiting"), error: (error, hasRecords) => calls.push([error.message, hasRecords]) }, { sessionFailure: id => id === "missing-native-rollout" ? failure : null });
  store.session = "missing-native-rollout"; store.generation = 1;
  globalThis.fetch = () => new Promise(resolve => { release = resolve; });
  try {
    const pending = store.poll(1);
    assert.equal(store.refreshFailure(), false);
    failure = { message: "No saved rollout exists" };
    t.mock.timers.tick(1200);
    assert.deepEqual(calls, [["No saved rollout exists", false]]);
    release({ ok: false, status: 404 }); await pending;
  } finally { store.stop(); globalThis.fetch = original; }
});

test("a known native failure skips missing pulls while unrelated and already-read sessions stay readable", async () => {
  const original = globalThis.fetch, calls = []; let pulls = 0;
  const store = new RecordStore({ error: error => calls.push(error.message) }, { sessionFailure: id => id === "missing" ? { message: "Native rejected this session" } : null });
  store.session = "missing"; store.generation = 1;
  globalThis.fetch = async () => { pulls++; return { ok: false, status: 404 }; };
  try {
    await store.poll(1);
    assert.equal(pulls, 0); assert.deepEqual(calls, ["Native rejected this session"]);
    store.records = [{ kind: "message" }]; assert.equal(store.refreshFailure(), false);
    store.records = []; store.session = "ordinary"; await store.poll(1);
    assert.equal(pulls, 1); assert.equal(calls.at(-1), "pull HTTP 404");
  } finally { store.stop(); globalThis.fetch = original; }
});

test("a first send waits for its missing rollout, then reads it without flashing an error", async t => {
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 1000 });
  const original = globalThis.fetch, calls = []; let empty = true;
  const store = new RecordStore({ empty: () => calls.push("empty"), materializing: () => calls.push("saving"), error: error => calls.push(error.message) }, { emptySession: () => empty });
  store.session = "new-session"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: false, status: 404 });
  try {
    await store.poll(1); clearTimeout(store.timer); empty = false;
    await store.poll(1); clearTimeout(store.timer);
    t.mock.timers.tick(3000);
    await store.poll(1); clearTimeout(store.timer);
    assert.deepEqual(calls, ["empty", "saving", "saving"]);
    store.apply = () => { store.records.push({ kind: "message" }); calls.push("read"); };
    globalThis.fetch = async () => ({ ok: true, json: async () => ({}) });
    await store.poll(1); clearTimeout(store.timer);
    assert.equal(calls.at(-1), "read"); assert.equal(store.confirmedEmpty, false);
    globalThis.fetch = async () => ({ ok: false, status: 404 });
    await store.poll(1); assert.equal(calls.at(-1), "pull HTTP 404", "a later deleted history is a real failure");
  } finally { store.stop(); globalThis.fetch = original; }
});

test("missing first history has a bounded grace period and never covers access or server failures", async t => {
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 1000 });
  const original = globalThis.fetch, errors = [];
  const store = new RecordStore({ error: error => errors.push(error.message) });
  store.session = "missing"; store.generation = 1; store.confirmedEmpty = true;
  try {
    globalThis.fetch = async () => ({ ok: false, status: 404 });
    await store.poll(1); clearTimeout(store.timer); assert.deepEqual(errors, []);
    t.mock.timers.tick(10001); await store.poll(1); clearTimeout(store.timer);
    assert.deepEqual(errors, ["pull HTTP 404"]);
    for (const status of [403, 500]) {
      store.missingSince = null;
      globalThis.fetch = async () => ({ ok: false, status });
      await store.poll(1); clearTimeout(store.timer); assert.equal(errors.at(-1), `pull HTTP ${status}`);
    }
    store.open("other"); assert.equal(store.confirmedEmpty, false); store.stop();
  } finally { store.stop(); globalThis.fetch = original; }
});

test("an older decoded pull cannot retire the new session's pending-history proof", async () => {
  const original = globalThis.fetch; let decode;
  const store = new RecordStore({}); store.session = "old"; store.generation = 1;
  globalThis.fetch = async () => ({ ok: true, json: () => new Promise(resolve => { decode = resolve; }) });
  try {
    const old = store.poll(1); while (!decode) await Promise.resolve();
    store.stop(); store.session = "new"; store.confirmedEmpty = true;
    decode({}); await old;
    assert.equal(store.confirmedEmpty, true); assert.equal(store.missingSince, null);
  } finally { store.stop(); globalThis.fetch = original; }
});
