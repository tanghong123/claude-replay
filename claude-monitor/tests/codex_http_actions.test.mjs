import assert from "node:assert/strict";
import { test } from "node:test";
import { createActionId, GatewayApi, GatewayError } from "../src/codex-workspace/api.js";

const uuidV4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

test("secure-context UUID generation retains the Crypto receiver", () => {
  const source = { randomUUID() { assert.equal(this, source); return "native-uuid"; }, getRandomValues() { throw new Error("Unexpected fallback"); } };
  assert.equal(createActionId(source), "native-uuid");
});

test("private HTTP uses random bytes and emits a version-4 UUID with the standard variant", () => {
  let reads = 0;
  const source = { getRandomValues(bytes) { assert.equal(this, source); assert.equal(bytes.length, 16); reads++; return bytes.fill(255); } };
  const id = createActionId(source);
  assert.equal(reads, 1); assert.match(id, uuidV4);
  assert.equal(id, "ffffffff-ffff-4fff-bfff-ffffffffffff");
});

test("private HTTP default action IDs reach requests while explicit retry IDs remain unchanged", async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "crypto"), requests = [];
  let value = 0;
  Object.defineProperty(globalThis, "crypto", { configurable: true, value: { getRandomValues: bytes => bytes.fill(value++) } });
  try {
    const api = new GatewayApi(async (_url, options) => { requests.push(JSON.parse(options.body)); return { ok: true, json: async () => ({ ok: true }) }; });
    await api.action("send", { threadId: "thread-a" });
    await api.action("send", { threadId: "thread-a" }, requests[0].actionId);
    await api.action("send", { threadId: "thread-a" });
    assert.match(requests[0].actionId, uuidV4); assert.match(requests[2].actionId, uuidV4);
    assert.equal(requests[1].actionId, requests[0].actionId); assert.notEqual(requests[2].actionId, requests[0].actionId);
    assert.equal(value, 2);
  } finally { if (descriptor) Object.defineProperty(globalThis, "crypto", descriptor); else delete globalThis.crypto; }
});

test("missing cryptographic randomness fails before a native write is dispatched", () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "crypto");
  Object.defineProperty(globalThis, "crypto", { configurable: true, value: {} });
  try {
    const api = new GatewayApi(() => { assert.fail("A write without a random action identity must not dispatch"); });
    assert.throws(() => api.action("send", {}), error => error instanceof GatewayError && error.code === "unavailable" && /secure action IDs/.test(error.message));
  } finally { if (descriptor) Object.defineProperty(globalThis, "crypto", descriptor); else delete globalThis.crypto; }
});
