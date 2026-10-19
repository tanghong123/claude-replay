import assert from "node:assert/strict";
import { test } from "node:test";
import { OutgoingMessages } from "../src/codex-workspace/outgoing.js";
const row = (id, text = "hello") => ({ id, kind: "user", src: text });
const payload = { threadId: "t", text: "hello" };
const snapshot = (records = []) => ({ session: "s", records, ready: true });

test("preview precedes HTTP receipt, and history cannot turn uncertainty into success", () => {
  const messages = new OutgoingMessages(); messages.begin("a", payload, snapshot());
  assert.equal(messages.list("t")[0].status, "sending");
  messages.observe("s", [row("new")]); assert.equal(messages.list("t").length, 1);
  messages.settle("a", "uncertain"); assert.equal(messages.list("t")[0].status, "uncertain");
  messages.begin("a", payload, snapshot()); assert.equal(messages.list("t").length, 1);
  messages.settle("a", "accepted"); assert.equal(messages.list("t").length, 0);
});
test("repeated text needs a distinct newly observed history record for each preview", () => {
  const messages = new OutgoingMessages();
  for (const id of ["a", "b"]) { messages.begin(id, payload, snapshot([row("old")])); messages.settle(id, "accepted"); }
  messages.observe("s", [row("old")]); assert.equal(messages.list("t").length, 2);
  messages.observe("s", [row("old"), row("new1")]); assert.equal(messages.list("t").length, 1);
  messages.observe("s", [row("old"), row("new1")]); assert.equal(messages.list("t").length, 1);
  messages.observe("s", [row("old"), row("new1"), row("new2")]); assert.equal(messages.list("t").length, 0);
});
test("rejection removes the preview; changing sessions cannot reconcile a different target", () => {
  const messages = new OutgoingMessages(); messages.begin("a", payload, snapshot()); messages.settle("a", "accepted");
  assert.deepEqual(messages.list("other"), []); messages.observe("other", [row("new")]); assert.equal(messages.list("t").length, 1);
  messages.begin("b", payload, snapshot()); messages.settle("b", "rejected"); assert.equal(messages.list("t").length, 1);
  messages.dismiss("a"); assert.equal(messages.list("t").length, 0);
});
test("cold unread history and image-only receipts require explicit dismissal without invented matches", () => {
  const messages = new OutgoingMessages(); messages.begin("cold", payload, { ...snapshot(), ready: false }); messages.settle("cold", "accepted");
  messages.observe("s", [row("old")]); assert.equal(messages.list("t").length, 1);
  messages.begin("image", { threadId: "i", text: "", images: ["data:image/png;base64,aA=="] }, snapshot()); messages.settle("image", "accepted");
  messages.observe("s", [row("empty", "")]); assert.equal(messages.list("i")[0].images.length, 1);
});
test("the native queue takes over accepted previews, including later cancellation", () => {
  const messages = new OutgoingMessages(); messages.begin("a", payload, snapshot()); messages.settle("a", "accepted");
  assert.deepEqual(messages.list("t", [{ clientUserMessageId: "a" }]), []);
  assert.deepEqual(messages.list("t", []), []);
});

test("old accepted previews retire even without text or a visited conversation", () => {
  const messages = new OutgoingMessages();
  messages.begin("uncertain", payload, snapshot()); messages.settle("uncertain", "uncertain");
  for (let i = 0; i < 80; i++) {
    messages.begin(`accepted-${i}`, { threadId: `other-${i}`, text: "", images: ["data:image/png;base64,aA=="] }, { ready: false });
    messages.settle(`accepted-${i}`, "accepted");
  }
  assert.equal(messages.entries.size, 33);
  assert.equal(messages.list("t")[0].status, "uncertain");
  assert.equal(messages.takeRetired().length, 48);
});
