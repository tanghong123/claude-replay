import assert from "node:assert/strict";
import { test } from "node:test";
import { nativeSessionGroups, nativeSessionTree, nativeProjectTree } from "../src/codex-workspace/monitor-model.js";
import { groupSessions, hideAction, visibleTree } from "../src/codex-ui/shared/session-visibility.js";

const research = { id: "research-project", name: "research", roots: [{ path: "/repos/research" }] };
const tree = (threads, projects = [research], legacy = { groups: [] }) => nativeSessionTree(nativeSessionGroups(legacy, threads, projects).groups, groupSessions)[0].projects;

test("assigned and legacy sessions share one sidebar project without rewriting native membership", () => {
  const threads = [
    { id: "new", cwd: "/repos/research", projectId: research.id, name: "test" },
    { id: "old", path: "/sessions/rollout-old.jsonl", cwd: "/repos/research", projectId: null, name: "Existing task" },
  ];
  const original = structuredClone(threads);
  for (const order of [threads, [...threads].reverse()]) {
    const [project, extra] = tree(order);
    assert.equal(extra, undefined);
    assert.equal(project.id, `native-project:${research.id}`);
    assert.equal(project.name, "research");
    assert.equal(project.path, "/repos/research");
    assert.equal(project.ignoreKey, "p:/repos/research");
    assert.deepEqual(project.sessions.map(row => row.nativeThreadId), order.map(thread => thread.id));
    assert.equal(project.sessions.find(row => row.nativeThreadId === "old").projectId, null);
    assert.equal(project.sessions.find(row => row.nativeThreadId === "old").id, "rollout-old");
  }
  assert.deepEqual(threads, original);
});

test("a unique directory group matches project roots and children with trailing separators", () => {
  const project = { ...research, roots: [{ path: "/repos/research/" }, { path: "/repos/research" }, { path: "/repos/notes/" }] };
  for (const cwd of ["/repos/research/", "/repos/research/topic", "/repos/notes"]) {
    const result = tree([{ id: "legacy", cwd }, { id: "assigned", cwd, projectId: research.id }], [project]);
    assert.equal(result.length, 1);
    assert.equal(result[0].name, "research");
    assert.deepEqual(result[0].sessions.map(row => row.nativeThreadId), ["legacy", "assigned"]);
    assert.equal(result[0].sessions[0].projectId, undefined);
  }
});

test("distinct legacy directory groups keep independent Hide/Restore targets and visibility", () => {
  for (const roots of [[{ path: "/repos/research" }, { path: "/repos/notes" }], [{ path: "/repos" }]]) {
    const project = { ...research, roots };
    const threads = [{ id: "hidden", path: "/sessions/hidden.jsonl", cwd: "/repos/research" }, { id: "visible", path: "/sessions/visible.jsonl", cwd: "/repos/notes" }];
    const legacy = { groups: [
      { ignoreKey: "p:/repos/research", hidden: true, rows: [{ id: "hidden", hidden: true }] },
      { ignoreKey: "p:/repos/notes", hidden: false, rows: [{ id: "visible", hidden: false }] },
    ] };
    for (const order of [threads, [...threads].reverse()]) {
      const projects = tree(order, [project], legacy);
      assert.equal(projects.length, 2);
      const [agent] = visibleTree([{ id: "codex", projects }]);
      assert.deepEqual(agent.projects.flatMap(group => group.rows.map(row => row.nativeThreadId)), ["visible"]);
      const hidden = projects.find(group => group.sessions[0].nativeThreadId === "hidden");
      const visible = projects.find(group => group.sessions[0].nativeThreadId === "visible");
      assert.deepEqual(hideAction(hidden, "project"), { op: "remove", key: "p:/repos/research", title: "Restore this project", icon: "back" });
      assert.deepEqual(hideAction(visible, "project"), { op: "add", key: "p:/repos/notes", title: "Hide this project", icon: "x" });
    }
  }
});

test("an assigned thread in another directory cannot absorb a legacy directory hide key", () => {
  const threads = [{ id: "legacy", cwd: "/repos/research/topic" }, { id: "assigned", cwd: "/repos/research", projectId: research.id }];
  const result = tree(threads);
  assert.deepEqual(result.map(project => project.id), ["p:/repos/research/topic", `native-project:${research.id}`]);
  assert.deepEqual(result.map(project => project.ignoreKey), ["p:/repos/research/topic", "p:/repos/research"]);
});

test("adding another project directory keeps each same-directory native and legacy pair merged", () => {
  const root = [{ id: "old-root", cwd: "/repos/research", projectId: null }, { id: "new-root", cwd: "/repos/research", projectId: research.id }];
  const child = [{ id: "new-child", cwd: "/repos/research/topic", projectId: research.id }, { id: "old-child", cwd: "/repos/research/topic", projectId: null }];
  for (const threads of [root, [...root, child[0]], [...child, ...root], [...root, ...child], root]) {
    const projects = tree(threads);
    const rootProject = projects.find(project => project.path === "/repos/research");
    assert.deepEqual(rootProject.sessions.map(row => row.nativeThreadId), ["old-root", "new-root"]);
    assert.equal(rootProject.ignoreKey, "p:/repos/research");
    assert.equal(rootProject.id, `native-project:${research.id}`);
    assert.equal(projects.length, threads.length === 2 ? 1 : 2);
    if (projects.length === 2) {
      const childProject = projects.find(project => project.path === "/repos/research/topic");
      assert.deepEqual(childProject.sessions.map(row => row.nativeThreadId), threads.filter(thread => thread.cwd === "/repos/research/topic").map(thread => thread.id));
      assert.equal(childProject.ignoreKey, "p:/repos/research/topic");
      assert.notEqual(rootProject.id, childProject.id);
    }
    assert.equal(rootProject.sessions[0].projectId, null);
  }
});

test("paired directories retain independent visibility and Restore actions under either thread order", () => {
  const threads = [
    { id: "old-root", path: "/sessions/old-root.jsonl", cwd: "/repos/research" },
    { id: "new-root", cwd: "/repos/research", projectId: research.id },
    { id: "old-child", path: "/sessions/old-child.jsonl", cwd: "/repos/research/topic" },
    { id: "new-child", cwd: "/repos/research/topic", projectId: research.id },
  ];
  const legacy = { groups: [
    { ignoreKey: "p:/repos/research", hidden: true, rows: [{ id: "old-root", hidden: true }] },
    { ignoreKey: "p:/repos/research/topic", hidden: false, rows: [{ id: "old-child", hidden: false }] },
  ] };
  for (const order of [threads, [...threads].reverse()]) {
    const projects = tree(order, [research], legacy);
    assert.equal(projects.length, 2);
    const [agent] = visibleTree([{ id: "codex", projects }]);
    assert.deepEqual(new Set(agent.projects.flatMap(group => group.rows.map(row => row.nativeThreadId))), new Set(["old-child", "new-child"]));
    const rootProject = projects.find(project => project.path === "/repos/research");
    assert.deepEqual(hideAction(rootProject, "project"), { op: "remove", key: "p:/repos/research", title: "Restore this project", icon: "back" });
  }
});

test("directory prefixes do not assign sibling directories to a project", () => {
  const [project] = tree([{ id: "sibling", cwd: "/repos/research-other" }]);
  assert.equal(project.id, "p:/repos/research-other");
  assert.equal(project.name, "research-other");
});

test("shared or nested project roots remain ambiguous regardless of catalog order", () => {
  for (const path of ["/repos/research", "/repos"]) {
    const other = { id: "other-project", name: "Other", roots: [{ path }] };
    const threads = [{ id: "one", cwd: "/repos/research", projectId: research.id }, { id: "two", cwd: "/repos/research", projectId: other.id }, { id: "legacy", cwd: "/repos/research" }];
    for (const projects of [[research, other], [other, research]]) {
      const result = tree(threads, projects);
      assert.deepEqual(result.map(project => project.id), [`native-project:${research.id}`, `native-project:${other.id}`, "p:/repos/research"]);
      assert.deepEqual(result.map(project => project.sessions.map(row => row.nativeThreadId)), [["one"], ["two"], ["legacy"]]);
    }
  }
});

test("explicit project membership wins even when the directory matches another project", () => {
  const other = { id: "other-project", name: "Other", roots: [{ path: "/repos/other" }] };
  const [project] = tree([{ id: "assigned", cwd: "/repos/research", projectId: other.id }], [research, other]);
  assert.equal(project.id, "native-project:other-project");
  assert.equal(project.name, "Other");
  assert.equal(project.sessions[0].projectId, other.id);
});

test("an unavailable explicit project is not reassigned by its directory", () => {
  const [project] = tree([{ id: "assigned", cwd: "/repos/research", projectId: "unavailable" }]);
  assert.equal(project.id, "p:/repos/research");
  assert.equal(project.sessions[0].projectId, "unavailable");
});

test("missing or invalid roots and directories retain directory grouping", () => {
  const projects = [{ ...research, roots: [{ path: "" }, { path: "relative/path" }, null] }, { id: "no-roots", name: "No roots" }];
  for (const cwd of [undefined, "relative/path", "/repos/research"]) {
    const [project] = tree([{ id: "unassigned", cwd }], projects);
    assert.equal(project.id, `p:${cwd}`);
    assert.equal(project.sessions[0].nativeThreadId, "unassigned");
  }
});

test("a root project matches absolute directories without turning missing paths into roots", () => {
  const projects = [{ id: "root", name: "Root", roots: [{ path: "/" }] }];
  assert.equal(tree([{ id: "absolute", cwd: "/repos/research" }], projects)[0].id, "native-project:root");
  assert.equal(tree([{ id: "missing" }], projects)[0].id, "p:undefined");
});

test("catalog changes recompute inferred display membership while session identities stay fixed", () => {
  const thread = { id: "legacy", cwd: "/repos/research", projectId: null };
  const other = { id: "other", name: "Other", roots: research.roots };
  const catalogs = [[research], [research, other], [research], [], [{ ...research, roots: [{ path: "/moved/research" }] }]];
  assert.deepEqual(catalogs.map(projects => tree([thread], projects)[0].id), [`native-project:${research.id}`, "p:/repos/research", `native-project:${research.id}`, "p:/repos/research", "p:/repos/research"]);
  for (const projects of catalogs) {
    const [row] = tree([thread], projects)[0].sessions;
    assert.equal(row.id, "native:legacy");
    assert.equal(row.nativeThreadId, "legacy");
    assert.equal(row.projectId, null);
  }
  assert.equal(thread.projectId, null);
});

test("merged legacy sessions preserve hidden rows, parser metadata and the Restore key", () => {
  const legacy = { groups: [{ kind: "project", label: "research", ignoreKey: "p:/repos/research", hidden: true, rows: [{ id: "rollout-old", hidden: true, family: "fork-family", cost: 12 }] }] };
  const projects = tree([{ id: "old", path: "/sessions/rollout-old.jsonl", cwd: "/repos/research" }, { id: "new", cwd: "/repos/research", projectId: research.id }], [research], legacy);
  const [project] = projects;
  assert.equal(projects.length, 1);
  assert.equal(project.sessions[0].family, "fork-family");
  assert.equal(project.sessions[0].cost, 12);
  assert.equal(project.sessions[0].hidden, true);
  assert.deepEqual(visibleTree([{ id: "codex", projects }]), []);
  assert.deepEqual(hideAction(project, "project"), { op: "remove", key: "p:/repos/research", title: "Restore this project", icon: "back" });
});


test("native project actions retain empty projects and filtered groups without exposing hidden projects", () => {
  const all = [{ id: "codex", projects: [{ id: "hidden", nativeProjectId: "hidden", hidden: true, sessions: [] }, { id: "filtered", nativeProjectId: "filtered", sessions: [{ id: "idle" }] }] }];
  const claude = { id: "claude", projects: [{ id: "ordinary", rows: [{ id: "c" }] }] };
  const result = nativeProjectTree([claude], all, [{ id: "hidden" }, { id: "filtered" }, research]);
  assert.deepEqual(result[0], claude);
  assert.deepEqual(result[1].projects.map(project => project.nativeProjectId), ["filtered", research.id]);
  assert.deepEqual(result[1].projects.map(project => project.rows), [[], []]);
  assert.equal(result[1].projects[1].path, "/repos/research");
  assert.equal(all[0].projects.length, 2);
});


test("a cold reader directory owns one saved-project action without a duplicate empty project", () => {
  const group = { id: "legacy", name: "research", path: "/repos/research", sessions: [], rows: [] };
  const all = [{ id: "codex", projects: [group] }];
  const [agent] = nativeProjectTree(all, all, [research]);
  assert.equal(agent.projects.length, 1);
  assert.equal(agent.projects[0].nativeProjectId, research.id);
  assert.equal(group.nativeProjectId, undefined);
  const ambiguous = nativeProjectTree(all, all, [research, { ...research, id: "other" }])[0].projects;
  assert.equal(ambiguous[0].nativeProjectId, undefined);
  assert.equal(ambiguous.length, 3, "same-directory projects stay distinct");
});
