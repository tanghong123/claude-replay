import assert from "node:assert/strict";
import test from "node:test";
import { DeliveryJournal } from "../src/codex-workspace/delivery-journal.js";
import { OutgoingMessages } from "../src/codex-workspace/outgoing.js";

function storage() {
  const scopes = new Map();
  let pending = Promise.resolve();
  return { change(scope, mutate) {
    const next = pending.then(() => {
      const value = structuredClone(scopes.get(scope) || { version: 1, attempts: [], drafts: {} });
      const result = mutate(value); scopes.set(scope, value); return result;
    });
    pending = next.catch(() => {}); return next;
  } };
}
const payload = { threadId: "t", text: "hello", images: ["data:image/png;base64,fixture"], intent: "next" };
const preview = { session: "rollout-t", baseline: [], canReconcile: true };

test("reload restores an in-flight identity and preview without authorizing another send", async () => {
  const disk = storage(), first = new DeliveryJournal("scope", "tab", disk);
  await first.draft("t", { text: "hello", images: [], revision: 3 });
  await first.prepare("original", payload, preview);
  const reloaded = new DeliveryJournal("scope", "tab", disk), saved = await reloaded.load();
  assert.equal(saved.drafts[JSON.stringify(["tab", "t"])].text, "hello");
  const messages = new OutgoingMessages(); messages.restore(saved.attempts[0]);
  assert.equal(messages.list("t")[0].status, "uncertain");
  assert.equal((await reloaded.prepare("new", payload, preview)).id, "original");
  assert.equal((await reloaded.prepare("new", payload, preview)).recovered, true);
});
test("two tabs atomically share an unresolved send while their drafts remain separate", async () => {
  const disk = storage(), a = new DeliveryJournal("scope", "a", disk), b = new DeliveryJournal("scope", "b", disk);
  const claims = await Promise.all([a.prepare("one", payload, preview), b.prepare("two", payload, preview)]);
  assert.equal(new Set(claims.map(entry => entry.id)).size, 1);
  assert.equal(claims.filter(entry => !entry.recovered).length, 1);
  await a.draft("t", { text: "a", images: [], revision: 1 }); await b.draft("t", { text: "b", images: [], revision: 2 });
  assert.equal(Object.keys((await a.load()).drafts).length, 2);
});
test("accepted repetition and explicitly retired uncertainty receive new identities", async () => {
  const journal = new DeliveryJournal("scope", "tab", storage());
  await journal.prepare("one", payload, preview); await journal.settle("one", "accepted");
  assert.equal((await journal.prepare("two", payload, preview)).id, "two");
  await journal.settle("two", "uncertain"); await journal.retire("two");
  assert.equal((await journal.prepare("three", payload, preview)).id, "three");
});
test("runtime scope isolates recovery and storage failure refuses a new identity", async () => {
  const disk = storage(), a = new DeliveryJournal("a", "tab", disk), b = new DeliveryJournal("b", "tab", disk);
  await a.prepare("one", payload, preview); assert.equal((await b.load()).attempts.length, 0);
  const error = new Error("Quota exceeded");
  const failed = new DeliveryJournal("scope", "tab", { change: async () => { throw error; } });
  await assert.rejects(failed.prepare("lost", payload, preview), error);
});
test("matching history explains uncertainty without treating repeated text as a receipt", () => {
  const messages = new OutgoingMessages(); messages.restore({ id: "one", payload, preview, status: "uncertain" });
  messages.observe("rollout-t", [{ id: "b1", kind: "user", src: "hello" }]);
  assert.equal(messages.list("t")[0].status, "uncertain"); assert.equal(messages.list("t")[0].historyMatch, true);
  messages.settle("one", "accepted"); assert.deepEqual(messages.list("t"), []);
  assert.deepEqual(messages.takeRetired(), ["one"]);
});

test("capacity refusal preserves every outstanding delivery identity", async () => {
  const journal = new DeliveryJournal("scope", "tab", storage());
  for (let i = 0; i < 64; i++) await journal.prepare(`action-${i}`, { ...payload, text: `message-${i}` }, preview);
  await assert.rejects(journal.prepare("overflow", { ...payload, text: "more" }, preview), /Review outstanding/);
  const saved = await journal.load();
  assert.equal(saved.attempts.length, 64);
  assert.equal(saved.attempts[0].id, "action-0");
  assert.equal((await journal.prepare("retry", payloadWithText("message-0"), preview)).id, "action-0");
});
function payloadWithText(text) { return { ...payload, text }; }

test("accepted image-only and unread previews cannot exhaust unresolved capacity", async () => {
  const journal = new DeliveryJournal("scope", "tab", storage());
  await journal.prepare("unresolved", payload, preview);
  for (let i = 0; i < 80; i++) {
    await journal.prepare(`accepted-${i}`, { ...payload, threadId: `other-${i}`, text: "" }, { ...preview, canReconcile: false });
    await journal.settle(`accepted-${i}`, "accepted");
  }
  const saved = await journal.load();
  assert.equal(saved.attempts.filter(entry => entry.status === "accepted").length, 32);
  assert.equal(saved.attempts[0].id, "unresolved");
  assert.equal(saved.attempts[1].id, "accepted-48");
  assert.equal((await journal.prepare("next", { ...payload, text: "next" }, preview)).id, "next");
});
test("a late uncertainty cannot downgrade another tab's accepted receipt", async () => {
  const journal = new DeliveryJournal("scope", "tab", storage());
  await journal.prepare("one", payload, preview); await journal.settle("one", "accepted");
  await journal.settle("one", "uncertain");
  assert.equal((await journal.load()).attempts[0].status, "accepted");
});
test("old other-tab drafts expire without dropping current drafts or unresolved sends", async () => {
  let now = 1000;
  const disk = storage(), old = new DeliveryJournal("scope", "old", disk, () => now);
  await old.draft("t", { text: "old", images: ["data:image/png;base64,fixture"] });
  await old.prepare("pending", payload, preview);
  const current = new DeliveryJournal("scope", "current", disk, () => now);
  await current.draft("t", { text: "keep", images: [] });
  now += 8 * 24 * 60 * 60 * 1000;
  const saved = await current.load();
  assert.deepEqual(Object.keys(saved.drafts), [JSON.stringify(["current", "t"])]);
  assert.equal(saved.attempts[0].id, "pending");
});
test("legacy drafts receive a grace period before another tab expires them", async () => {
  let now = 1000;
  const disk = storage();
  await disk.change("scope", value => { value.drafts[JSON.stringify(["old", "t"])] = { text: "legacy", images: [] }; });
  const journal = new DeliveryJournal("scope", "current", disk, () => now);
  assert.equal(Object.keys((await journal.load()).drafts).length, 1);
  now += 8 * 24 * 60 * 60 * 1000;
  assert.equal(Object.keys((await journal.load()).drafts).length, 0);
});
