import assert from "node:assert/strict";
import { test } from "node:test";
import { monitorSessionId, nativeSessionGroups, nativeSessionTree, tierOptions } from "../src/codex-workspace/monitor-model.js";
import { groupSessions, hideAction, visibleTree } from "../src/codex-ui/shared/session-visibility.js";
import { sessionBucket, needsPerson, denoteState, displayState } from "../src/codex-ui/shared/state-labels.js";
import { modelOptions } from "../src/codex-workspace/model.js";

test("native path identifies the exact original Monitor stream, independent of UUID-shaped filename suffixes", () => {
  assert.equal(monitorSessionId({ id: "native-a", path: "/home/sessions/rollout-t-native-a_suffix.jsonl" }), "rollout-t-native-a_suffix");
  assert.equal(monitorSessionId({ id: "new" }), "native:new");
});

test("native history owns visibility and project identity while original parser metadata survives", () => {
  const old = { groups: [{ rows: [{ id: "rollout-t-native-a", agent: "codex", name: "old", family: "family", cost: 12, state: "idle" }, { id: "foreign", agent: "codex", name: "Must disappear" }] }] };
  const threads = [{ id: "native-a", path: "/home/rollout-t-native-a.jsonl", cwd: "/repo", projectId: "p", name: "Native title", status: { type: "active" }, updatedAt: 3 }, { id: "new", cwd: "/repo", projectId: "p" }];
  const result = nativeSessionGroups(old, threads, [{ id: "p", name: "Project" }]);
  assert.equal(result.groups.length, 1); assert.equal(result.groups[0].label, "Project");
  assert.equal(result.groups[0].rows.length, 2);
  const row = result.groups[0].rows[0];
  assert.equal(row.nativeThreadId, "native-a"); assert.equal(row.cost, 12); assert.equal(row.family, "family"); assert.equal(row.name, "Native title"); assert.equal(row.state, "growing");
  assert.equal(result.groups[0].rows.some(row => row.id === "foreign"), false);
});

test("Fast picks the native advertised tier and preserves Standard as explicit null", () => {
  const [model] = modelOptions([{ id: "m", serviceTiers: [{ id: "priority", name: "Fast", description: "2x speed" }, null], defaultServiceTier: null }]);
  assert.deepEqual(tierOptions(model), [{ id: null, name: "Standard", description: "Standard speed" }, { id: "priority", name: "Fast", description: "2x speed" }]);
  assert.deepEqual(tierOptions(modelOptions([{ id: "unsupported" }])[0]), [{ id: null, name: "Standard", description: "Standard speed" }]);
});

test("overlapping native project roots keep distinct project identities without changing Monitor hide keys", () => {
  const groups = nativeSessionGroups({ groups: [] }, [
    { id: "a", cwd: "/repo", projectId: "one" }, { id: "b", cwd: "/repo", projectId: "two" },
    { id: "c", cwd: "/repo/nested", projectId: "one" },
  ], [{ id: "one", name: "One" }, { id: "two", name: "Two" }]).groups;
  const [agent] = nativeSessionTree(groups, groupSessions);
  assert.deepEqual(agent.projects.map(project => project.name), ["One", "Two"]);
  assert.deepEqual(agent.projects.map(project => project.sessions.map(row => row.nativeThreadId)), [["a", "c"], ["b"]]);
  assert.notEqual(agent.projects[0].id, agent.projects[1].id);
  assert.equal(agent.projects[0].ignoreKey, "p:/repo");
});

test("native idle status retires stale busy metadata from an earlier Monitor scan", () => {
  const result = nativeSessionGroups({ groups: [{ rows: [{ id: "rollout", state: "growing", agentState: "busy" }] }] }, [
    { id: "thread", path: "/home/rollout.jsonl", cwd: "/repo", status: { type: "idle" } },
  ], []);
  assert.equal(result.groups[0].rows[0].state, "idle");
  assert.equal(result.groups[0].rows[0].agentState, "idle");
});


test("native waiting flags and completion update every original Monitor state consumer together", () => {
  let row = { id: "rollout", state: "growing", agentState: "busy", stateReason: "tool", stateConfidence: "inferred" };
  const advance = status => {
    row = nativeSessionGroups({ groups: [{ rows: [row] }] }, [{ id: "thread", path: "/home/rollout.jsonl", cwd: "/repo", updatedAt: 20, status }], []).groups[0].rows[0];
    return row;
  };
  advance({ type: "active", activeFlags: ["waitingOnApproval"] });
  assert.equal(sessionBucket(row), "blocked"); assert.equal(needsPerson(row), true);
  assert.deepEqual(denoteState(row), { label: "Awaiting permission", tone: "wait" });
  advance({ type: "active", activeFlags: ["waitingOnUserInput"] });
  assert.equal(sessionBucket(row), "blocked"); assert.equal(displayState(row).label, "Awaiting an answer");
  advance({ type: "active", activeFlags: [] });
  assert.equal(sessionBucket(row), "active"); assert.equal(needsPerson(row), false);
  advance({ type: "idle" });
  assert.equal(sessionBucket(row), "idle"); assert.equal(needsPerson(row), false);
  assert.deepEqual(denoteState(row, 0), { label: "New result", tone: "unread" }); assert.equal(denoteState(row, 20), null);
  row.stateReason = "permission"; row.agentState = "wait";
  advance({ type: "idle" }); assert.equal(sessionBucket(row), "idle"); assert.equal(displayState(row).label, "Done");
  advance({ type: "systemError" }); assert.equal(sessionBucket(row), "blocked"); assert.equal(needsPerson(row), true);
});


test("current observed terminal failures or questions survive native idle but older transcript evidence retires", () => {
  for (const reason of ["ended-question", "error", "failed", "stalled", "exited-mid-work"]) {
    const previous = { id: "rollout", state: "idle", agentState: "idle", stateReason: reason, stateConfidence: "observed", stateDetail: "Current transcript evidence", activityTs: 20 };
    const row = updatedAt => nativeSessionGroups({ groups: [{ rows: [previous] }] }, [{ id: "thread", path: "/home/rollout.jsonl", cwd: "/repo", updatedAt, status: { type: "idle" } }], []).groups[0].rows[0];
    assert.equal(sessionBucket(row(20)), "blocked"); assert.equal(row(20).stateDetail, previous.stateDetail);
    assert.equal(sessionBucket(row(21)), "idle"); assert.equal(row(21).stateReason, "done");
  }
});


test("a hidden native project retains Monitor visibility and its original Restore action", () => {
  const legacy = { groups: [{ kind: "project", label: "repo", ignoreKey: "p:/repo", hidden: true, rows: [{ id: "rollout", hidden: true }] }] };
  const native = nativeSessionGroups(legacy, [{ id: "thread", path: "/home/rollout.jsonl", cwd: "/repo", projectId: "project" }], [{ id: "project", name: "Project" }]);
  const tree = nativeSessionTree(native.groups, groupSessions);
  assert.deepEqual(visibleTree(tree), []);
  const [agent] = visibleTree(tree, { showHidden: true });
  assert.equal(agent.projects[0].hidden, true);
  assert.equal(agent.projects[0].sessions[0].hidden, true);
  assert.deepEqual(hideAction(agent.projects[0], "project"), { op: "remove", key: "p:/repo", title: "Restore this project", icon: "back" });
});
