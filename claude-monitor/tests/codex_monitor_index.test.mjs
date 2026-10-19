import assert from "node:assert/strict";
import { test } from "node:test";
globalThis.localStorage = { getItem: () => null };
globalThis.matchMedia = () => ({ matches: false });
globalThis.document = { body: { dataset: {} } };
const { SessionIndexStore } = await import("../src/codex-ui/session-index-store.js");
const { indexState } = await import("../src/codex-ui/state.js");

test("a filter or creation refresh during a scan waits for current rows and discards the stale scan", async () => {
  let release, loads = 0;
  const first = new Promise(resolve => { release = resolve; });
  const observed = [];
  const store = new SessionIndexStore({ update: () => observed.push([...indexState.rows.keys()]) }, {
    load: async () => ++loads === 1 ? first : { groups: [{ rows: [{ id: "current-project-session" }] }] },
  });
  try {
    const initial = store.refresh();
    const latest = store.refresh();
    assert.equal(initial, latest);
    release({ groups: [{ rows: [{ id: "stale-project-session" }] }] });
    await latest;
    assert.equal(loads, 2);
    assert.deepEqual(observed, [["current-project-session"]]);
  } finally { store.stop(); indexState.rows = new Map(); indexState.groups = []; }
});

test("stopping while a scan is in flight never arms another poll", async () => {
  let release;
  const store = new SessionIndexStore({}, { load: () => new Promise(resolve => { release = resolve; }) });
  const pending = store.refresh(); store.stop();
  release({ groups: [] }); await pending;
  assert.equal(store.loading, null); assert.equal(store.timer, 0);
});

for (const failure of [false, true]) test(`changing scan source releases an old pending native scan (failure: ${failure})`, async () => {
  let settle;
  const seen = [], errors = [];
  const store = new SessionIndexStore({ update: () => seen.push([...indexState.rows.keys()]), error: error => errors.push(error) }, {
    load: () => new Promise((resolve, reject) => { settle = () => failure ? reject(new Error("native_not_enabled")) : resolve({ groups: [{ rows: [{ id: "obsolete" }] }] }); }),
  });
  try {
    const old = store.refresh(); store.stop();
    store.load = async () => ({ groups: [{ rows: [{ id: "ordinary" }] }] });
    await store.start();
    assert.deepEqual(seen, [["ordinary"]], "ordinary history must load without waiting for the stopped native scan");
    settle(); await old;
    assert.deepEqual(seen, [["ordinary"]]); assert.deepEqual(errors, []);
    store.load = async () => { throw new Error("real current scan failure"); };
    await store.refresh(); assert.equal(errors[0].message, "real current scan failure");
  } finally { store.stop(); indexState.rows.clear(); indexState.groups = []; }
});
