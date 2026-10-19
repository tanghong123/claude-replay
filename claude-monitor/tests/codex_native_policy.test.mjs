import assert from "node:assert/strict";
import { test } from "node:test";
import { usesNativeControls } from "../src/codex-ui/native-policy.js";
import { nativeSessionGroups } from "../src/codex-workspace/monitor-model.js";

test("ordinary reads and Write mode do not activate native controls", () => {
  for (const row of [null, { agent: "claude" }, { agent: "codex" }, { agent: "Codex", nativeThreadId: "thread" }]) {
    for (const write of [false, true]) assert.equal(usesNativeControls("off", write, row), false);
    assert.equal(usesNativeControls("enabling", true, row), false);
  }
});

test("native selection is gated by Write mode and leaves Claude on ordinary controls", () => {
  assert.equal(usesNativeControls("on", true, { agent: "codex" }), true);
  assert.equal(usesNativeControls("on", false, { agent: "codex" }), false);
  assert.equal(usesNativeControls("on", true, { agent: "claude" }), false);
  assert.equal(usesNativeControls("on", true, { agent: "claude" }, "create"), true);
  assert.equal(usesNativeControls("on", true, { agent: "claude" }, "none"), false);
  assert.equal(usesNativeControls("on", true, null), true);
  assert.equal(usesNativeControls("on", true, { agent: "unknown" }), false);
});

test("native history preserves Claude groups, worker metadata and original visibility", () => {
  const row = { id: "claude", agent: "claude", name: "Claude session", startedBy: "parent", injectable: true, hidden: true };
  const group = { kind: "project", label: "Claude project", ignoreKey: "p:/claude", hidden: true, rows: [row] };
  const result = nativeSessionGroups({ groups: [group], ignoredCount: 3 }, [{ id: "native", cwd: "/codex" }], []);
  assert.deepEqual(result.groups[0], group);
  assert.equal(result.groups[0].rows[0], row);
  assert.equal(result.ignoredCount, 3);
  assert.equal(result.groups[1].rows[0].nativeThreadId, "native");
});
