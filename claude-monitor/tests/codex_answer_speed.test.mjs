import assert from "node:assert/strict";
import { test } from "node:test";
import { answerRate, turnReading } from "../src/codex-ui/shared/time.js";
import { freshCursor, pullQuery } from "../src/codex-ui/shared/record-stream.js";
import { RecordStore } from "../src/codex-ui/record-store.js";
import { VirtualWindow } from "../src/codex-ui/shared/virtual-window.js";

test("turn readings cover duration, speed, tooltip, and missing values", () => {
  const answer = { tok: 1472, ms: 46000, n: 2, model: "gpt-example" };
  assert.equal(answerRate(answer), "32 tok/s");
  const both = turnReading(46, answer);
  assert.equal(both.text, "worked 46s · 32 tok/s");
  assert.match(both.title, /1,472 output tokens in 46\.0 s of generation \(2 answers, gpt-example; includes first-token wait, excludes tool time between answers\)/);
  assert.equal(turnReading(null, answer).text, "32 tok/s");
  assert.equal(turnReading(46, null).text, "worked 46s");
  assert.equal(turnReading(null, {tok: 0, ms: 1}), null);
  assert.equal(turnReading(null, {tok: 1, ms: 0}), null);
  assert.equal(answerRate({tok: 1, ms: 10000}), "<1 tok/s");
  assert.match(turnReading(null, {...answer, n: 1}).title, /1 answer,/);
});

test("positive short intervals never appear as zero seconds", () => {
  for (const [ms, duration] of [[1, "<0.1"], [49, "<0.1"], [50, "0.1"], [100, "0.1"]]) {
    assert.ok(turnReading(null, {tok: 1, ms, n: 1, model: "m"}).title.includes(`in ${duration} s of generation`));
  }
});

test("the independent metadata generation round trips and refreshes only provisional openers", () => {
  assert.equal(pullQuery("s", freshCursor()), "session=s&cursor=0.0.0.0");
  assert.equal(pullQuery("s", freshCursor(), 3), "session=s&cursor=0.0.0.0&meta=3");
  const maxRevision = JSON.parse('{"meta_gen":9007199254740991}').meta_gen;
  assert.equal(maxRevision, Number.MAX_SAFE_INTEGER);
  assert.equal(pullQuery("s", freshCursor(), maxRevision), "session=s&cursor=0.0.0.0&meta=9007199254740991");
  assert.equal(pullQuery("s", freshCursor(), maxRevision + 1), "session=s&cursor=0.0.0.0");
  const updates = [];
  const store = new RecordStore({update: update => updates.push(update)});
  store.apply({epoch:1, committed_from:0, committed:[{kind:"user",turn:1,ans:{tok:1}}],
    provisional_gen:1, provisional_from:0, provisional:[{kind:"user",turn:2,ans:{tok:2}}, {kind:"peer",turn:3}],
    meta_gen:maxRevision, meta:{open_turn_ans:[{turn:1,tok:40,ms:1000,n:1,model:"m"}]}});
  assert.equal(store.metaGen, maxRevision);
  assert.equal(store.records[0].ans.tok, 1);
  assert.equal(store.records[1].ans.tok, 40);
  assert.equal(store.records[2].ans, undefined);
  store.apply({epoch:1, committed_from:1, committed:[], provisional_gen:1, provisional_from:2, provisional:[],
    meta_gen:4, meta:{open_turn_ans:[{turn:2,tok:90,ms:3000,n:1,model:"m"}]}});
  assert.equal(store.records[1].ans, undefined, "a missing open reading clears a stale initial reading");
  assert.equal(store.records[2].ans.tok, 90);
  assert.equal(store.records[0].ans.tok, 1, "committed readings stay frozen");
  assert.equal(updates.length, 2);
  store.recover(); assert.equal(store.metaGen, null);
});

test("queue dequeue replaces the provisional queue with the command and retains the override on remount", async () => {
  const { Projection } = await import("../src/codex-ui/view-model.js");
  const store = new RecordStore({});
  const user = { kind: "user", id: "u", turn: 1, body: [], dur: 46 };
  const queue = { kind: "queue", id: "q", body: [] };
  const command = { kind: "command", id: "c", turn: 2, head: { name: "review" }, body: [], dur: 30 };
  const reading = { turn: 1, tok: 64, ms: 2000, n: 1, model: "m" };
  store.apply({ epoch: 1, committed_from: 0, committed: [], provisional_gen: 1,
    provisional_from: 0, provisional: [user, queue, command], meta_gen: 1, meta: {open_turn_ans: [reading]} });
  const projection = new Projection();
  projection.rebuild(store.records);
  store.apply({ epoch: 1, committed_from: 0, committed: [user], provisional_gen: 2,
    provisional_from: 0, provisional: [{...command}], meta_gen: 2, meta: {open_turn_ans: [reading]} });
  assert.deepEqual(store.records.map(record => record.kind), ["user", "command"]);
  projection.rebuild(store.records, 0);
  const commandUnit = projection.units.find(unit => unit.view?.source?.kind === "command");
  assert.equal(commandUnit.view.source.ans.tok, 64);
  store.apply({ epoch: 1, committed_from: 1, committed: [], provisional_gen: 2,
    provisional_from: 1, provisional: [], meta_gen: 3, meta: {open_turn_ans: [{...reading, tok: 80}]} });
  assert.equal(commandUnit.view.source.ans.tok, 80, "a kept projection uses the updated source on remount");
  assert.equal(turnReading(commandUnit.view.source.dur, commandUnit.view.source.ans).text, "worked 30s · 40 tok/s");
});

test("queued records transactions pass their completion callback after replacement", () => {
  const viewport = Object.create(VirtualWindow.prototype);
  Object.defineProperty(viewport, "count", { get: () => records.length });
  viewport.tailSnapshot = () => null;
  let transaction;
  viewport.transact = (_cause, options) => { transaction = options; return false; };
  let records = ["user", "queue", "command"], mounted = [...records], refreshed = false;
  viewport.recordsChanged(() => { records = ["user", "command"]; return 1; }, () => {
    assert.deepEqual(mounted, records);
    refreshed = true;
  });
  assert.equal(refreshed, false, "a queued transaction must not refresh the stale queue card");
  transaction.mutate();
  assert.equal(refreshed, false);
  mounted = [...records];
  transaction.after();
  assert.equal(refreshed, true);
});
