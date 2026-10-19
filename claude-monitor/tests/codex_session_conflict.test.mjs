import assert from "node:assert/strict";
import { test } from "node:test";
import { isThreadInUse } from "../src/codex-workspace/session-conflict.js";

test("only an exact resume ownership refusal selects the recovery UI", () => {
  const error = (code, message) => ({ code: "rejected", message: `thread/resume: ${JSON.stringify({ code, message })}` });
  assert.equal(isThreadInUse({ code: "thread_in_use" }, "a"), true);
  assert.equal(isThreadInUse({ code: "thread_in_use" }, null), false);
  assert.equal(isThreadInUse(error(-32600, "thread a already has an active writer"), "a"), true);
  assert.equal(isThreadInUse(error(-32600, "thread b already has an active writer"), "a"), false);
  assert.equal(isThreadInUse(error(-32601, "thread a already has an active writer"), "a"), false);
  assert.equal(isThreadInUse({ code: "rejected", message: "thread/resume: invalid JSON" }, "a"), false);
  assert.equal(isThreadInUse({ ...error(-32600, "thread a already has an active writer"), code: "uncertain" }, "a"), false);
  assert.equal(isThreadInUse({ code: "rejected", message: 'turn/start: {"code":-32600,"message":"thread a already has an active writer"}' }, "a"), false);
});

const { sharedServerSetup } = await import("../src/codex-workspace/session-conflict.js");
test("Unix setup copies actual socket/home context without unrelated secrets or personal preferences", () => {
  const setup = sharedServerSetup({ transport: "unix", endpoint: "unix:///tmp/host/rpc.sock", socketPath: "/tmp/host/rpc.sock", codexHome: "/tmp/host/codex", authentication: { type: "unix-permissions", token: "NEVER_COPY_THIS" }, model: "personal-model", fast: false });
  assert.match(setup.prompt, /unix:\/\/\/tmp\/host\/rpc.sock/);
  assert.match(setup.prompt, /Configured CODEX_HOME: "\/tmp\/host\/codex"/);
  assert.doesNotMatch(setup.prompt, /NEVER_COPY_THIS|personal-model|Fast|Bearer/);
  assert.equal(setup.rows[1][1], "/tmp/host/rpc.sock");
});
test("WebSocket setup provides actual port and token file, and states App compatibility limits", () => {
  const setup = sharedServerSetup({ transport: "websocket", endpoint: "ws://127.0.0.1:49123", codexHome: "/tmp/home", authentication: { type: "bearer", tokenFile: "/tmp/state/server.token", token: "NEVER_COPY_THIS" } });
  assert.match(setup.prompt, /49123/); assert.match(setup.prompt, /\/tmp\/state\/server.token/);
  assert.match(setup.prompt, /Authorization: Bearer/); assert.match(setup.prompt, /user-run migration.*private Unix socket/);
  assert.doesNotMatch(setup.prompt, /NEVER_COPY_THIS/);
});
test("both setup prompts hand off a runnable command without stopping their own Codex chat", () => {
  for (const status of [
    { transport: "unix", endpoint: "unix:///tmp/home/rpc.sock", socketPath: "/tmp/home/rpc.sock", codexHome: "/tmp/home", authentication: { type: "unix-permissions" } },
    { transport: "websocket", endpoint: "ws://127.0.0.1:49123", codexHome: "/tmp/home", authentication: { type: "bearer", tokenFile: "/tmp/state/server.token" } },
  ]) {
    const { prompt } = sharedServerSetup(status);
    assert.match(prompt, /one-line terminal command/);
    assert.match(prompt, /Do not execute it or quit\/stop any App or server from this chat/);
    assert.match(prompt, /save work and confirm any restart/);
    assert.match(prompt, /preserve unrelated instances/);
    assert.match(prompt, /verify the actual shared connection/);
  }
});
test("missing or incompatible metadata cannot produce a guessed setup prompt", () => {
  for (const status of [null, {}, { endpoint: "unix:///x", codexHome: "/y", transport: "unix", socketPath: "/x", authentication: { type: "bearer" } }]) {
    assert.equal(sharedServerSetup(status), null);
  }
});
