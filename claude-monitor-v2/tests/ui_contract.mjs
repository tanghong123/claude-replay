import assert from "node:assert/strict";
import { readFileSync, readdirSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RecordStore } from "../../claude-monitor/src/codex-ui/record-store.js";
import { promptShouldCollapse, rawTurnHtml, rendererStartsClosed } from "../../claude-monitor/src/codex-ui/components.js";
import { attachmentCapability, referenceAction, revealQuery, stampQuery } from "../../claude-replay-html/src/html/shared/capabilities.js";
import { RUNTIME_ALWAYS, runtimeRows, runtimeText } from "../../claude-replay-html/src/html/shared/runtime.js";
import { snipId } from "../../claude-replay-html/src/html/shared/ids.js";
import { DEFAULT_READING, READING_KEY, SIZE_MIN, clampSize, loadReading, parseReading, readingVars } from "../../claude-replay-html/src/html/shared/reading.js";
import { KEYMAP, hintFor, isEditable, resolveKey } from "../../claude-replay-html/src/html/shared/keymap.js";
import { agentRecordTargets, currentTurnIndex, Projection, taskRecordTargets, taskStatus, viewRecord, taskOrder, taskGroups, taskGroupKey, taskCenterTarget, taskDetails } from "../../claude-monitor/src/codex-ui/view-model.js";
import { revealNavigationContext } from "../../claude-monitor/src/codex-ui/viewport.js";
import { PREVIEW_CSP, sandboxDocument } from "../../claude-monitor/src/codex-ui/sandbox.js";
import { families, familyKey, groupSessions, groupVisible, hideAction, ignoreQuery, rowVisible, visibleTree } from "../../claude-replay-html/src/html/shared/session-visibility.js";
import { REASONS, denoteState, displayState, needsPerson, stateTip } from "../../claude-replay-html/src/html/shared/state-labels.js";
import { composeCapability, composeCopy, consentQuery, grantOutcome, runRevoke, runSend, sendOutcome, sendQuery } from "../../claude-replay-html/src/html/shared/control-protocol.js";
import { cursorText, freshCursor, parseRecords, pullQuery, recordsQuery, reducePull } from "../../claude-replay-html/src/html/shared/record-stream.js";
import { parseViewMemory, serializeViewMemory, viewMemoryKey } from "../../claude-monitor/src/codex-ui/view-memory.js";

const demo = readFileSync(new URL("../../design/agent-monitor-codex-demo.html", import.meta.url), "utf8");
const referenceCss = readFileSync(new URL("../../claude-monitor/src/codex-ui/reference.css", import.meta.url), "utf8");
const referenceShell = readFileSync(new URL("../../claude-monitor/src/codex-ui/reference-shell.html", import.meta.url), "utf8");
const productionCss = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
const viewportSource = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
const appSource = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
const previewSource = readFileSync(new URL("../../claude-monitor/src/codex-ui/preview.js", import.meta.url), "utf8");
const extractedCss = `${demo.slice(demo.indexOf("<style>\n") + 8, demo.indexOf("\n</style>", demo.indexOf("<style>\n")))}\n`;
const extractedShell = demo.slice(demo.indexOf("<body>\n") + 7, demo.indexOf('<script src="sample-transcript-data.js"></script>'));
assert.equal(referenceCss, extractedCss, "reference CSS must remain an exact demo extraction");
assert.equal(referenceShell, extractedShell, "reference shell must remain an exact demo extraction");
assert.match(productionCss, /\.monitor-empty\[hidden\]\{display:none!important\}/, "a loaded session must not retain the loading placeholder in layout");
assert.match(viewportSource, /USER_INTENT_MS/, "follow state must distinguish user scroll intent");
assert.match(viewportSource, /captureDomAnchor/, "window changes must preserve a real DOM anchor");
assert.match(viewportSource, /reconcile\(/, "scrolling must incrementally reconcile the bounded window");
assert.doesNotMatch(viewportSource, /behavior:\s*smooth/, "tail following must converge without cancellable smooth scrolling");
assert.match(productionCss, /markdown-table-scroll>table\{display:table!important;width:100%!important/, "production markdown tables must fill their scroll viewport");
assert.match(productionCss, /\.prompt-attachments\{/, "production-only prompt attachments must have a dedicated layout");
assert.match(productionCss, /\.input-request\{/, "native input history must not render as raw JSON");
assert.match(productionCss, /\.turn\.proposed-plan\{/, "semantic proposed plans must have a dedicated surface");
assert.match(productionCss, /\.fence-h\{/, "safe markdown fences must keep their toolbar inside the code surface");
assert.match(productionCss, /\.image-lightbox\{/, "image attachments must have an in-page lightbox");
assert.match(productionCss, /image-lightbox\[data-state="unavailable"\]/, "missing temporary images must collapse to a compact recovery state");
assert.match(productionCss, /\.tree-project-more\{/, "projects with many sessions need an inline disclosure control");
assert.match(productionCss, /\.prompt-copy-shell\.collapsed/, "long prompts need a bounded collapsed surface");
assert.match(productionCss, /\.spot-link\{/, "message permalinks need a subtle production treatment");
assert.match(appSource, /url\.hash = id/, "message permalinks must copy a session URL with a stable record hash");
assert.match(appSource, /function landOnHash\(\)/, "record hashes must land through the virtual viewport");
assert.match(viewportSource, /jumpToRecord\(recordIndex, reveal = "record"\)/, "deep links must materialize virtualized records before scrolling");
assert.match(viewportSource, /reveal = "record"/, "navigation must carry an explicit reveal depth");
assert.match(viewportSource, /processExpanded\.add\(process\.key\)/, "turn navigation must reveal the following process list");
assert.match(viewportSource, /folds\.set\(target\.id, false\)/, "task, agent and search navigation must open the target execution block");
assert.match(appSource, /SIDEBAR_SESSION_LIMIT = 5/, "the sidebar must initially bound each project to five sessions");
assert.match(previewSource, /setSession\(sessionId\)/, "preview tabs must be scoped by session");
assert.match(previewSource, /SESSION_CACHE_LIMIT = 6/, "restored preview state must remain session-bounded");
assert.match(previewSource, /SESSION_CACHE_BYTES = 12 \* 1024 \* 1024/, "restored preview payloads must have a memory budget");
assert.equal(promptShouldCollapse("short prompt"), false);
assert.equal(promptShouldCollapse("x".repeat(561)), true);
assert.equal(rendererStartsClosed({ renderer: "bash", running: false, interaction: null }), true);
assert.equal(rendererStartsClosed({ renderer: "read", running: true, interaction: null }), false);
assert.equal(rendererStartsClosed({ renderer: "tool", running: false, interaction: { kind: "request_user_input" } }), false);
assert.equal(rendererStartsClosed({ renderer: "queue", running: false, interaction: null }), false);
const embeddedImage = attachmentCapability({ att_kind: "image", att_name: "shot.png", att_datauri: "data:image/png;base64," });
assert.equal(embeddedImage.action, "image");
assert.match(embeddedImage.hint, /saved with the session/);
assert.match(attachmentCapability({ att_kind: "image", att_name: "shot.png", att_path: "/tmp/shot.png", att_fsig: "file-signed" }).hint, /temporary file/);
assert.equal(attachmentCapability({ att_name: "notes.md", att_path: "/tmp/notes.md", att_fsig: "file-signed" }).action, "preview");
assert.equal(attachmentCapability({ att_name: "sample.tgz", att_path: "/tmp/sample.tgz", att_fsig: "file-signed" }).action, "download");
{
  // Reveal (parity #2): a reveal stamp alone yields the file-manager action — never anything
  // that would call /file — and no stamp at all still degrades to copying the path.
  const revealOnly = attachmentCapability({ att_name: "notes.md", att_path: "/tmp/notes.md", att_sig: "reveal-only" });
  assert.equal(revealOnly.action, "reveal", "a reveal signature must never authorize /file");
  assert.equal(attachmentCapability({ att_kind: "image", att_name: "shot.png", att_path: "/tmp/shot.png", att_sig: "reveal-only" }).action, "reveal", "…for images too");
  assert.ok(!["preview", "download", "image"].includes(revealOnly.action));
  assert.equal(attachmentCapability({ att_name: "notes.md", att_path: "/tmp/notes.md", att_fsig: "file-signed", att_sig: "reveal-signed" }).action, "preview", "with both stamps the readable action stays primary");
  assert.equal(referenceAction({ fileSig: "f", revealSig: "r" }), "preview");
  assert.equal(referenceAction({ fileSig: "", revealSig: "r" }), "reveal");
  assert.equal(referenceAction({}), "copy");
  assert.equal(revealQuery({ path: "/w/my repo/a.rs", sig: "s+1" }), "/__reveal?path=%2Fw%2Fmy%20repo%2Fa.rs&sig=s%2B1", "the path and stamp travel verbatim, encoded once");
}
assert.equal(attachmentCapability({ att_name: "sample.tgz", att_path: "/tmp/sample.tgz" }).action, "copy");

for (const moduleName of ["app.js", "control-store.js", "preview.js"]) {
  const module = readFileSync(new URL(`../../claude-monitor/src/codex-ui/${moduleName}`, import.meta.url), "utf8");
  for (const [, id] of module.matchAll(/byId\("([^"]+)"\)/g)) {
    assert.match(referenceShell, new RegExp(`id=["']${id}["']`), `${moduleName} requires missing shell #${id}`);
  }
}

const updates = [];
const store = new RecordStore({ update: update => updates.push(update) });
store.apply({ epoch: 1, committed_from: 0, committed: [
  { t: "block", kind: "user", id: "u1", turn: 1, label: "First", body: [{ p: "md", h: "<p>First</p>" }] },
  { t: "block", kind: "assistant", id: "c1", phase: "commentary", body: [{ p: "md", h: "<p>Working</p>" }] }
], provisional_gen: 1, provisional_from: 0, provisional: [], meta: { tasks: [], children: [] } });
assert.deepEqual(store.records.map(record => record.id), ["u1", "c1"]);

store.apply({ epoch: 1, committed_from: 2, committed: [], provisional_gen: 2, provisional_from: 0, provisional: [
  { t: "block", kind: "bash", id: "b1", head: { name: "Bash" }, body: [{ p: "pre", x: "ok" }] },
  { t: "block", kind: "assistant", id: "f1", phase: "final", body: [{ p: "md", h: "<p>Done</p>" }] }
], meta: null });
assert.deepEqual(store.records.map(record => record.id), ["u1", "c1", "b1", "f1"]);

store.apply({ epoch: 1, committed_from: 2, committed: [], provisional_gen: 3, provisional_from: 0, provisional: [
  { t: "block", kind: "think", id: "t2", body: [{ p: "think", h: "<p>Retry</p>" }] }
], meta: null });
assert.deepEqual(store.records.map(record => record.id), ["u1", "c1", "t2"], "generation rewrite truncates the stale provisional tail");

store.apply({ epoch: 2, committed_from: 0, committed: [
  { t: "block", kind: "user", id: "u2", turn: 1, body: [{ p: "md", h: "<p>New epoch</p>" }] }
], provisional_gen: 0, provisional_from: 0, provisional: [], meta: null });
assert.deepEqual(store.records.map(record => record.id), ["u2"], "epoch reset drops the previous stream");
store.recover();
assert.deepEqual(store.records, [], "409 recovery drops records tied to the stale epoch");
assert.deepEqual(store.cursor, { epoch: 0, committed: 0, gen: 0, index: 0 });

const fixtures = [
  "user", "assistant", "think", "act", "bash", "read", "write", "edit", "skill", "tool",
  "agent", "task", "queue", "command", "compaction", "attachment", "unknown-kind"
].map((kind, index) => ({ t: "block", kind, id: `r${index}`, phase: kind === "assistant" ? "final" : undefined, head: { name: kind }, body: [{ p: "md", h: `<p>${kind}</p>` }] }));
const projection = new Projection();
projection.rebuild(fixtures, 0);
assert.ok(projection.units.length > 0);
assert.equal(viewRecord(fixtures.at(-1)).renderer, "fallback");
const claudeNested = viewRecord({ kind: "act", id: "thinking-run", head: { summary: "Thought, read 1 file" }, body: [{ p: "blocks", items: [{ kind: "read", id: "nested-read", head: { name: "Read", target: "src/main.rs" }, body: [{ p: "pre", x: "fn main() {}" }] }, { kind: "bash", id: "nested-bash", head: { name: "Bash", target: "cargo check" }, body: [{ p: "pre", x: "Finished" }] }] }] });
assert.deepEqual(claudeNested.children.map(child => child.renderer), ["read", "bash"], "Claude thinking/activity keeps nested tool order and component types");
const richProjection = new Projection();
richProjection.rebuild([
  { kind: "user", id: "prompt", turn: 1, label: "Inspect this", body: [{ p: "md", h: "<p>Inspect this</p>" }] },
  { kind: "attachment", id: "image", head: { att_kind: "image", att_name: "shot.png", att_path: "/tmp/shot.png", att_fsig: "file-signed", att_sig: "reveal-signed" }, body: [] },
  { kind: "assistant", id: "plan", phase: "final", head: { presentation: "proposed_plan" }, body: [{ p: "md", h: "<p>Plan</p>" }] },
  { kind: "tool", id: "input", head: { name: "request_user_input", interaction: { kind: "request_user_input", resolved: true, answers: [{ id: "choice", label: "Use v2" }] } }, body: [{ p: "pre", x: "raw" }] }
], 0);
assert.equal(richProjection.units[0].attachments.length, 1, "adjacent attachments belong to their prompt instead of a fake process");
assert.equal(richProjection.units[0].to, 1, "the prompt unit owns its attachment record for stable virtualization");
assert.equal(richProjection.units[1].view.presentation, "proposed_plan");
assert.equal(richProjection.units[2].views[0].view.interaction.answers[0].label, "Use v2");
const incrementalPrompt = new Projection();
const promptRecords = [{ kind: "user", id: "late-prompt", turn: 1, body: [{ p: "md", h: "<p>Prompt</p>" }] }];
incrementalPrompt.rebuild(promptRecords, 0);
promptRecords.push({ kind: "attachment", id: "late-image", head: { att_kind: "image", att_name: "late.png" }, body: [] });
incrementalPrompt.rebuild(promptRecords, 1);
assert.equal(incrementalPrompt.units.length, 1, "a later pull must rebuild only the owning prompt unit");
assert.equal(incrementalPrompt.units[0].attachments[0].id, "late-image");
assert.equal(taskStatus("Completed"), "completed");
assert.equal(taskStatus("InProgress"), "in_progress");
const taskTargets = taskRecordTargets(
  [{ id: "0", subject: "Extract immutable demo CSS" }, { id: "1", subject: "Wire real sessions" }],
  [{ kind: "tool", head: { name: "TodoWrite" }, body: [{ p: "pre", x: "Completed: Extract immutable demo CSS\nCompleted: Wire real sessions" }] }]
);
assert.deepEqual([...taskTargets.entries()], [["0", 0], ["1", 0]], "task rows navigate to their latest source record");
const agentTargets = agentRecordTargets(
  [{ id: "child-1" }, { id: "nested-child" }],
  [
    { kind: "agent", id: "spawn", head: { child_id: "child-1" }, body: [] },
    { kind: "act", id: "run", body: [{ p: "blocks", items: [{ kind: "agent", id: "nested", head: { child: "?session=nested-child" }, body: [] }] }] }
  ]
);
assert.deepEqual([...agentTargets.entries()], [["child-1", 0], ["nested-child", 1]], "agent outline rows resolve to their parent execution record");
const navigationUnits = [
  { type: "user", key: "user:1", turn: 1, from: 0, to: 0 },
  { type: "process", key: "process:1", turn: 1, from: 1, to: 3, views: [
    { index: 1, view: { id: "todo", t: "tool" } },
    { index: 2, view: { id: "already-open", t: "tool" } }
  ] }
];
const navigationState = { folds: new Map([["already-open", false]]), processFolds: new Map([["process:1", true]]), processExpanded: new Set(), promptExpanded: new Set() };
revealNavigationContext(navigationUnits, 0, navigationState, 0, "turn");
assert.equal(navigationState.processFolds.get("process:1"), false, "turn navigation opens its following process");
assert.equal(navigationState.processExpanded.has("process:1"), true, "turn navigation reveals progressive events");
assert.equal(navigationState.folds.get("already-open"), false, "navigation preserves a manual expand-all state");
revealNavigationContext(navigationUnits, 1, navigationState, 1, "task");
assert.equal(navigationState.folds.get("todo"), false, "task navigation opens the exact execution block");
revealNavigationContext(navigationUnits, 0, navigationState, 0, "search");
assert.equal(navigationState.promptExpanded.has("user:1"), true, "search reveals a long prompt before highlighting it");
assert.ok(updates.length >= 4);

console.log("ui contract fixtures passed");

// The HTML preview's policy is placed by rule, never by searching the artifact. The case that
// found the bug: a `<head>` inside a leading comment made the old regex drop the policy INTO
// the comment, and the frame ran with none.
const META = `<meta http-equiv="Content-Security-Policy" content="${PREVIEW_CSP}">`;
const policyIndex = html => sandboxDocument(html).indexOf(META);
assert.equal(policyIndex("<html><head><title>x</title></head><body>hi</body></html>"), 0, "no doctype: policy first");
assert.equal(policyIndex("<!-- <head> --><html><head></head><body>x</body></html>"), 0, "a <head> in a comment never becomes the insertion point");
{
  // A COMPLETE comment may precede the doctype (the tokenizer ends it exactly where the rule
  // does), so the policy lands after the doctype — outside the comment, standards mode kept.
  const src = "<!--\n<head>\n--><!DOCTYPE html><html><head></head></html>";
  const at = policyIndex(src);
  assert.equal(at, "<!--\n<head>\n--><!DOCTYPE html>".length, "a complete comment then a doctype: policy right after the doctype");
  assert.ok(at > src.indexOf("-->"), "…and never inside the comment");
  assert.equal(sandboxDocument(src).indexOf("<!--"), 0, "the artifact's own bytes are untouched");
}
assert.equal(policyIndex("<!-- x --!><!doctype html><html></html>"), 0, "an unusual comment terminator falls back to first, never inside");
{
  const doc = sandboxDocument("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"></head><body>y</body></html>");
  assert.equal(doc.indexOf(META), "<!DOCTYPE html>".length, "a leading doctype is kept ahead of the policy so the artifact stays in standards mode");
  assert.equal(doc.indexOf("<!DOCTYPE html>"), 0);
}
assert.equal(policyIndex("\uFEFF  <!doctype HTML SYSTEM 'about:legacy-compat'><html></html>"), "\uFEFF  <!doctype HTML SYSTEM 'about:legacy-compat'>".length, "BOM, whitespace and a legacy doctype are all still 'the doctype first'");
assert.equal(policyIndex(""), 0, "an empty artifact is just the policy");
assert.match(PREVIEW_CSP, /connect-src 'none'/);
assert.match(PREVIEW_CSP, /default-src 'none'/);
console.log("sandbox document cases passed");

// Hide / restore (parity #1): hidden is the server's state and a VIEW filter here — a hidden
// row is never dropped from the model, only from the tree, and the keys are the server's.
{
  const groups = [
    { kind: "project", label: "repo", secondary: "/w/repo", ignoreKey: "p:/w/repo", hidden: false, rows: [
      { id: "s1", agent: "claude", name: "one", ignoreKey: "s:s1", hidden: false, state: "growing" },
      { id: "s2", agent: "claude", name: "two", ignoreKey: "s:s2", hidden: true, state: "finished" }
    ] },
    { kind: "project", label: "old", secondary: "/w/old", ignoreKey: "p:/w/old", hidden: true, rows: [
      { id: "s3", agent: "claude", name: "three", ignoreKey: "s:s3", hidden: true, state: "finished" }
    ] },
    { kind: "agent", label: "QoderWork", ignoreKey: "a:QoderWork", hidden: false, rows: [
      { id: "q1", agent: "qoderwork", name: "desk", ignoreKey: "s:q1", hidden: false, state: "finished" }
    ] }
  ];
  const agents = groupSessions(groups);
  const claude = agents.find(a => a.id === "claude");
  assert.deepEqual(claude.projects.map(p => [p.id, p.ignoreKey, p.hidden]), [["p:/w/repo", "p:/w/repo", false], ["p:/w/old", "p:/w/old", true]], "projects carry the server's key and hidden flag verbatim");
  const desk = agents.find(a => a.id === "qoderwork").projects[0];
  assert.equal(desk.name, "Desktop sessions"); assert.equal(desk.kind, "agent"); assert.equal(desk.ignoreKey, "a:QoderWork", "an agent-kind group keeps its a: key so it can be hidden and restored");
  assert.equal(claude.projects[1].sessions.length, 1, "a hidden row stays in the model");

  const shown = visibleTree(agents);
  assert.deepEqual(shown.find(a => a.id === "claude").projects.map(p => [p.id, p.rows.map(r => r.id)]), [["p:/w/repo", ["s1"]]], "by default a hidden row and a hidden project vanish");
  const revealed = visibleTree(agents, { showHidden: true });
  assert.deepEqual(revealed.find(a => a.id === "claude").projects.map(p => [p.id, p.rows.map(r => r.id)]), [["p:/w/repo", ["s1", "s2"]], ["p:/w/old", ["s3"]]], "Hidden (n) reveals both");
  const needy = visibleTree(agents, { showHidden: true, attention: true, needs: r => r.id === "s2" });
  assert.deepEqual(needy.map(a => a.id), ["claude"]); assert.deepEqual(needy[0].projects.map(p => p.rows.map(r => r.id)), [["s2"]], "attention composes with reveal");
  assert.deepEqual(visibleTree(agents, { attention: true, needs: r => r.id === "s2" }), [], "…but never resurrects a hidden row on its own");

  assert.deepEqual(hideAction({ ignoreKey: "s:s1", hidden: false }), { op: "add", key: "s:s1", title: "Hide this session", icon: "x" });
  assert.deepEqual(hideAction({ ignoreKey: "p:/w/old", hidden: true }, "project"), { op: "remove", key: "p:/w/old", title: "Restore this project", icon: "back" });
  assert.equal(hideAction({ ignoreKey: "a:QoderWork", hidden: false }, "agent").title, "Hide this agent");
  assert.equal(ignoreQuery({ op: "add", key: "p:/w/my repo" }), "/api/ignore?add=p%3A%2Fw%2Fmy%20repo", "the key travels verbatim, encoded once");
  console.log("session visibility cases passed");
}
assert.match(appSource, /ignoreQuery\(\{ op, key \}\)/, "the shell hides and restores through /api/ignore with the server's key");
assert.match(appSource, /visibleTree\(agents, \{ showHidden: indexState\.showHidden/, "the tree is filtered through the tested predicate, not ad hoc");
assert.match(productionCss, /\.tree-action\{/, "row actions have a production treatment");
assert.match(productionCss, /\.tree-row\.is-hidden\{/, "a revealed hidden row is visibly different");
assert.match(appSource, /referenceAction\(\{ fileSig, revealSig \}\)/, "reference clicks dispatch on the tested precedence");
assert.match(readFileSync(new URL("../../claude-monitor/src/codex-ui/attachment-viewer.js", import.meta.url), "utf8"), /revealQuery\(\{ path: item\.path, sig: item\.sig \}\)/, "reveal calls /__reveal with the REVEAL stamp, never the file stamp");
assert.match(productionCss, /\.renderer-target-link\{/, "stamped tool-header paths read as references");
console.log("reveal cases passed");

// Fork families (parity #4): one representative per shared root, the root itself when it is
// among the rows, else the newest member; growing and activity are the family's, not the rep's.
{
  const rows = [
    { id: "root", family: "root", activityTs: 100, state: "finished" },
    { id: "fork1", family: "root", isFork: true, activityTs: 300, state: "growing" },
    { id: "fork2", family: "root", isFork: true, activityTs: 200, state: "finished" },
    { id: "lone", family: "lone", activityTs: 50, state: "finished" },
    { id: "orphanFork", family: "gone-root", isFork: true, activityTs: 10, state: "finished" },
    { id: "orphanFork2", family: "gone-root", isFork: true, activityTs: 20, state: "finished" }
  ];
  const fams = families(rows);
  assert.deepEqual(fams.map(f => [f.key, f.rep.id, f.forks.map(r => r.id), f.growing, f.latest]), [
    ["root", "root", ["fork1", "fork2"], true, 300],
    ["lone", "lone", [], false, 50],
    ["gone-root", "orphanFork2", ["orphanFork"], false, 20]
  ], "root represents; a family without its root is represented by its newest member; order is first appearance");
  assert.equal(families([]).length, 0);
  assert.equal(families([{ id: "x" }])[0].key, "x", "a row without a family field is a family of one, keyed by its id");
  console.log("family cases passed");
}
assert.match(appSource, /families\(rows\)/, "the tree clusters fork families through the tested function");
assert.match(appSource, /indexState\.selectedWasRow && !selectedRow\(\)\) sessionGone\(\)/, "a sub-agent child, never a list row, is not declared gone by the index poll");
assert.match(appSource, /meta\?\.ancestors\?\.at\(-1\)/, "the parent control reads the last ancestor from the session's own meta");
assert.match(productionCss, /\.session-parent\.is-live\{display:grid\}/, "the reference parent control is shown by a production state, not by editing the shell");
console.log("sub-agent navigation cases passed");
assert.match(appSource, /const first = requested \|\| \[\.\.\.indexState\.rows\.values\(\)\]/, "a ?session= deep link to a non-row id (a sub-agent child) opens that id, not the first row");

// Scroll position across reloads (parity #6): following is the position; an anchor is a unit
// key plus its offset; anything else is not a memory at all.
{
  assert.equal(viewMemoryKey("s1"), "am-view:s1");
  assert.deepEqual(parseViewMemory(serializeViewMemory({ following: true })), { following: true });
  assert.deepEqual(parseViewMemory(serializeViewMemory({ following: false, key: "user:b12", top: 17.6 })), { following: false, key: "user:b12", top: 18 }, "an anchor round-trips, rounded to the pixel");
  assert.equal(parseViewMemory(""), null); assert.equal(parseViewMemory("nope"), null); assert.equal(parseViewMemory('{"key":"","top":1}'), null); assert.equal(parseViewMemory('{"key":"x","top":"1"}'), null, "a non-numeric offset is rejected");
  assert.equal(parseViewMemory('{"following":false}'), null, "not following without an anchor is nothing to restore");
  assert.match(viewportSource, /beginSession\(session\)/, "the viewport reads the memory when a session opens");
  assert.match(viewportSource, /addEventListener\("pagehide", \(\) => this\.remember\(\)\)/, "the last position is written on the way out");
  assert.match(viewportSource, /this\.state\.following = !this\.pending/, "a remembered anchor holds following off until it is applied, so the tail never flashes past");
  assert.match(appSource, /viewport\.beginSession\(id\); recordStore\.open\(id\)/, "the memory is read before the stream starts");
  console.log("view memory cases passed");
}

// Reading controls (parity #7) and the raw toggle (parity #8).
{
  assert.deepEqual(parseReading(""), DEFAULT_READING); assert.deepEqual(parseReading("garbage"), DEFAULT_READING);
  assert.deepEqual(parseReading('{"size":"14.3","wrap":true,"wide":"yes"}'), { size: 14.5, wrap: true, wide: false }, "size snaps to half steps; flags must be real booleans");
  assert.equal(clampSize(3), 8, "#45: the range is the classic page's, 8–16"); assert.equal(clampSize(99), 16); assert.equal(clampSize(undefined), 12);
  assert.deepEqual(readingVars({ size: 11, wide: true }), { "--code-size": "11px", "--measure": "1240px" });
  assert.deepEqual(readingVars(DEFAULT_READING), { "--code-size": "12px", "--measure": "820px" });
  const raw = rawTurnHtml({ kind: "user", body: [{ p: "md", h: "<p>x</p>" }] });
  assert.match(raw, /^<pre class="turn-raw">/); assert.ok(raw.includes("&lt;p&gt;x&lt;\/p&gt;") || raw.includes("&lt;p&gt;x&lt;/p&gt;"), "the raw record is escaped, never re-entered as markup");
  assert.match(productionCss, /#app\{--code-size:12px;--measure:820px\}/, "reading preferences are custom properties on the app root");
  assert.match(productionCss, /#app\.wrap-code \.markdown pre/, "wrap is a class on the app root");
  assert.match(readFileSync(new URL("../../claude-monitor/src/codex-ui/state.js", import.meta.url), "utf8"), /if \(uiState\.readingChosen\) localStorage\.setItem\(READING_KEY, JSON\.stringify\(uiState\.reading\)\)/, "reading preferences persist with the other production preferences — once chosen (#45)");
  assert.match(appSource, /recordState\.rawTurns\.clear\(\)/, "raw turns reset when a session opens");
  console.log("reading + raw cases passed");
}

// Keyboard (parity #11): one table, the classic keys, never while typing.
{
  const seen = new Set();
  for (const b of KEYMAP) { const id = `${b.key}|${!!b.shift}|${b.when}`; assert.ok(!seen.has(id), `bound twice: ${id}`); seen.add(id); }
  for (const key of ["/", "[", "]", "j", "k", "n", "N", "w", "-", "+", "=", " ", "ArrowDown", "ArrowUp"]) assert.ok(KEYMAP.some(b => b.key === key), `classic key ${JSON.stringify(key)} is bound`);
  const ev = (key, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...extra });
  assert.equal(resolveKey(ev("]"), "view").action, "turn-next");
  assert.equal(resolveKey(ev("]"), "list"), null, "view keys do not fire while the list has focus");
  assert.equal(resolveKey(ev("ArrowDown"), "list").action, "list-next");
  assert.equal(resolveKey(ev("ArrowDown"), "view"), null, "arrow keys are the list's alone");
  assert.equal(resolveKey(ev("/"), "list").action, "search", "search works from anywhere");
  assert.equal(resolveKey(ev(" "), "view").action, "page-down"); assert.equal(resolveKey(ev(" ", { shiftKey: true }), "view").action, "page-up");
  assert.equal(resolveKey(ev(" "), "view", { tagName: "BUTTON" }), null, "Space on a button is the button's");
  assert.equal(resolveKey(ev("j"), "view", { tagName: "INPUT" }), null, "never while typing");
  assert.equal(resolveKey(ev("j"), "view", { tagName: "DIV", isContentEditable: true }), null);
  assert.equal(resolveKey(ev("k", { metaKey: true }), "view"), null, "platform shortcuts pass through");
  assert.ok(isEditable({ tagName: "TEXTAREA" }) && !isEditable({ tagName: "DIV" }));
  assert.equal(hintFor("hit-prev"), "N"); assert.equal(hintFor("page-up"), "⇧Space"); assert.equal(hintFor("nope"), "");
  assert.match(appSource, /bindKeymap\(document, /, "the shell binds the keymap once, at the document");
  assert.match(appSource, /viewport\.lastUserInput = performance\.now\(\)/, "key-driven scrolling counts as the reader's own, so following releases instead of snapping back");
  console.log("keymap cases passed");
}

// Seam (a) (#43): the classic rail and the v2 splice read the SAME predicates as the app
// shell — clustering, the hidden filter, the hide/restore action and its query — inlined
// into their pages at serve time, so none of them keeps a copy.
{
  assert.equal(familyKey({ id: "q1", agent: "qoderwork", name: "Plan the thing (Fork)" }), "t:Plan the thing", "QoderWork clusters by title, the rail's #153 rule");
  assert.equal(familyKey({ id: "q2", agent: "QoderWork", name: "  ", family: "root" }), "root", "an empty title falls back to the family root");
  assert.equal(familyKey({ id: "c1", agent: "claude", name: "Anything (Fork)", family: "c0" }), "c0", "other agents use the server's family");
  assert.equal(familyKey({ id: "c2", agent: "claude" }), "c2", "a row without a family is its own");
  const titled = families([
    { id: "q1", agent: "qoderwork", name: "Plan (Fork)", isFork: true, activityTs: 5 },
    { id: "q0", agent: "qoderwork", name: "Plan", activityTs: 3 }
  ]);
  assert.equal(titled.length, 1, "one family under the title");
  assert.equal(titled[0].rep.id, "q0", "the root represents it");
  assert.equal(titled[0].latest, 5, "the family's age is its newest member's");
  assert.equal(rowVisible({ hidden: true }), false);
  assert.equal(rowVisible({ hidden: true }, { showHidden: true }), true);
  assert.equal(rowVisible({ id: "a" }, { attention: true, needs: r => r.id === "b" }), false);
  assert.equal(groupVisible({ hidden: true }), false);
  assert.equal(groupVisible({ hidden: true }, { showHidden: true }), true);
  assert.equal(groupVisible({}), true);
  const railSource = readFileSync(new URL("../../claude-monitor/src/rail.html", import.meta.url), "utf8");
  const spliceSource = readFileSync(new URL("../src/shell.html", import.meta.url), "utf8");
  for (const [name, src] of [["rail.html", railSource], ["shell.html", spliceSource]]) {
    assert.match(src, /\{\{SHARED\}\}/, `${name} carries the inlined shared modules`);
    assert.doesNotMatch(src, /function (families|clusterKey)\(/, `${name} keeps no clustering of its own`);
  }
  assert.match(railSource, /var families=shared\.families;/, "the rail clusters through the shared function");
  assert.match(railSource, /shared\.hideAction\(/, "the rail's hide/restore controls come from hideAction");
  assert.match(railSource, /shared\.ignoreQuery\(/, "the rail hides and restores through the shared query");
  assert.match(railSource, /shared\.rowVisible\(/, "the rail's row filter is the shared predicate");
  assert.match(railSource, /shared\.groupVisible\(/, "the rail's group filter is the shared predicate");
  assert.match(spliceSource, /__shared\.rowVisible\(/, "the splice's hidden filter is the shared predicate");
  console.log("seam (a) cases passed");
}

// Seam (f) (#44): one state-label table. Every reason claude-replay-engine's StateReason
// emits is worded once; the app shell's chip/status and the rail's tooltip read that word.
{
  const engineReasons = ["exited", "exited-mid-work", "question", "plan-approval", "queued-prompt", "tool", "thinking", "permission", "ended-question", "error", "done", "starting", "stalled"];
  assert.deepEqual([...REASONS].sort(), [...engineReasons].sort(), "the table lists exactly the engine's reasons (StateReason::as_str)");
  for (const reason of engineReasons) {
    const { label } = displayState({ agentState: "idle", stateReason: reason });
    assert.ok(label && label !== reason, `${reason} has wording`);
  }
  assert.deepEqual(displayState({ state: "growing" }), { state: "busy", reason: "growing", label: "Running" }, "legacy growing reads as busy");
  assert.deepEqual(displayState({ state: "finished" }), { state: "idle", reason: "exited", label: "Done" }, "legacy finished reads as exited");
  assert.equal(displayState({ agentState: "wait", stateReason: "permission" }).label, "Awaiting permission");
  assert.equal(needsPerson({ agentState: "wait", stateReason: "permission" }), true);
  assert.equal(needsPerson({ agentState: "idle", stateReason: "stalled" }), true);
  assert.equal(needsPerson({ agentState: "busy", stateReason: "tool" }), false);
  assert.deepEqual(denoteState({ agentState: "busy", stateReason: "tool" }), { label: "Running", tone: "busy" });
  assert.deepEqual(denoteState({ agentState: "wait", stateReason: "question", stateConfidence: "inferred" }), { label: "Awaiting an answer", tone: "wait inferred" });
  assert.deepEqual(denoteState({ agentState: "idle", stateReason: "ended-question" }), { label: "Awaiting reply", tone: "attention" });
  assert.deepEqual(denoteState({ agentState: "idle", stateReason: "exited-mid-work" }), { label: "Exited abnormally", tone: "danger" });
  assert.deepEqual(denoteState({ agentState: "idle", stateReason: "exited", activityTs: 10 }, 5), { label: "New result", tone: "unread" });
  assert.equal(denoteState({ agentState: "idle", stateReason: "exited", activityTs: 10 }, 10), null, "read rows carry no marker");
  assert.equal(stateTip({ state: "finished", visited: true }), "Done — no growth, no process");
  assert.equal(stateTip({ state: "growing", visited: true }), "Running — the transcript grew since the last scan");
  assert.equal(stateTip({ state: "idle", conf: "unconfirmed", ambig: 3, visited: true }), "Idle — a live agent is in this directory — but it was started without a session id, and `--resume` picks from a list, so it may be driving any of these 3 sessions");
  assert.equal(stateTip({ state: "idle", agentState: "wait", stateReason: "permission", visited: true }), "Awaiting permission — alive — a live process names this session");
  assert.match(stateTip({ state: "finished" }), /lazy fold\)$/, "an unvisited row says its counters are not folded yet");
  assert.match(appSource, /from "\.\/shared\/state-labels\.js"/, "the shell imports the shared table");
  assert.doesNotMatch(appSource, /const labels = \{ busy:/, "the shell keeps no label table of its own");
  const railSrc = readFileSync(new URL("../../claude-monitor/src/rail.html", import.meta.url), "utf8");
  assert.match(railSrc, /shared\.stateTip\(/, "the rail's tooltip is the shared one");
  assert.doesNotMatch(railSrc, /finished — no growth, no process/, "the rail keeps no tooltip wording of its own");
  const spliceSrc = readFileSync(new URL("../src/shell.html", import.meta.url), "utf8");
  assert.match(spliceSrc, /__shared\.stateTip\(/, "the splice's tooltip is the shared one");
  console.log("seam (f) cases passed");
}

// Seam (d) (#45): keymap.js and reading.js are shared; the classic page and the rail resolve
// keys through the one table and keep reading preferences under the one key.
{
  assert.equal(SIZE_MIN, 8, "the range is the classic page's (8–16 in half steps)");
  assert.equal(clampSize(7), 8); assert.equal(clampSize(12.3), 12.5); assert.equal(clampSize(99), 16);
  assert.deepEqual(parseReading(null, { size: 12.5, wrap: true, wide: false }), { size: 12.5, wrap: true, wide: false }, "a page's own defaults apply");
  assert.deepEqual(parseReading('{"size":14}', { size: 12.5, wrap: true, wide: false }), { size: 14, wrap: true, wide: false }, "missing fields fall back to the defaults, not to false");
  assert.deepEqual(parseReading('{"size":14,"wrap":false}'), { size: 14, wrap: false, wide: false });
  const store = new Map([["claude-replay-export-ms", "14"], ["claude-replay-export-wrap", "0"]]);
  const get = k => (store.has(k) ? store.get(k) : null), set = (k, v) => store.set(k, v), del = k => store.delete(k);
  const legacy = { size: "claude-replay-export-ms", wrap: "claude-replay-export-wrap", wide: "claude-replay-export-wide" };
  const migrated = loadReading(get, set, { size: 12.5, wrap: true, wide: false }, legacy, del);
  assert.deepEqual(migrated, { size: 14, wrap: false, wide: false }, "the pre-#45 keys are folded in once");
  assert.equal(store.get(READING_KEY), JSON.stringify({ size: 14, wrap: false, wide: false }), "…and written under the one key");
  assert.equal(store.has("claude-replay-export-ms"), false, "…and the legacy keys are removed");
  assert.deepEqual(loadReading(get, set, { size: 12.5, wrap: true, wide: false }, legacy, del), migrated, "the one key wins from then on");
  const empty = new Map();
  assert.deepEqual(loadReading(k => empty.get(k) ?? null, (k, v) => empty.set(k, v), { size: 12.5, wrap: true, wide: false }, legacy), { size: 12.5, wrap: true, wide: false }, "nothing stored → the page's defaults");
  assert.equal(empty.size, 0, "…and nothing is written until the reader chooses");
  const exportSource = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(exportSource, /shared\.resolveKey\(e, "view", e\.target\)/, "the classic page resolves keys through the shared table");
  assert.doesNotMatch(exportSource, /e\.key === "\]"|e\.key === "w"|e\.key === "j"/, "the classic page keeps no key chain of its own");
  assert.match(exportSource, /shared\.loadReading\(/, "the classic page loads prefs through the shared loader");
  assert.doesNotMatch(exportSource, /var MS_KEY|var WRAP_KEY|var WIDE_KEY/, "the classic page keeps no pref keys of its own");
  const railSrc2 = readFileSync(new URL("../../claude-monitor/src/rail.html", import.meta.url), "utf8");
  assert.match(railSrc2, /shared\.resolveKey\(/, "the rail resolves keys through the shared table");
  assert.match(appSource, /from "\.\/shared\/keymap\.js"/); assert.match(appSource, /from "\.\/shared\/reading\.js"/);
  // The shared key is written only by a CHOICE, in either shell — never by a load or by an
  // unrelated persist() carrying one shell's defaults into the other.
  const stateSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/state.js", import.meta.url), "utf8");
  assert.match(stateSrc, /if \(uiState\.readingChosen\) localStorage\.setItem\(READING_KEY/, "the app shell persists reading prefs only once chosen");
  assert.match(appSource, /uiState\.readingChosen = true; persist\(\)/, "…and setReading is the choice");
  assert.match(exportSource, /\n  applyMono\(ms\);\n  applyWide\(wide\);/, "the classic page applies stored prefs at load without persisting them");
  assert.doesNotMatch(exportSource, /\n  setMono\(ms\);|\n  setWide\(wide\);/, "…never through the persisting setters");
  console.log("seam (d) cases passed");
}

// Seam (b) (#46): the two-stamp file rule is shared, and the classic page's two click sites
// call it — the first export.js consumer of seam 0.
{
  assert.equal(stampQuery({ path: "/w/my repo/a.txt", sig: "s1" }), "path=%2Fw%2Fmy%20repo%2Fa.txt&sig=s1");
  assert.equal(stampQuery({ path: "/w/a.txt" }), "path=%2Fw%2Fa.txt", "no stamp, no sig parameter");
  assert.equal(revealQuery({ path: "/w/a.txt", sig: "s1" }), "/__reveal?" + stampQuery({ path: "/w/a.txt", sig: "s1" }), "the app shell's query is the same form behind the route");
  assert.equal(referenceAction({ fileSig: null, revealSig: "r" }), "reveal", "a reveal stamp never authorizes /file");
  const exportSrc = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.equal((exportSrc.match(/shared\.referenceAction\(\{ fileSig: ARTIFACTS \? /g) || []).length, 3, "the attachment card, the path link's title and the path click all ask the rule");
  assert.doesNotMatch(exportSrc, /ARTIFACTS && fsig|ARTIFACTS && tp\.dataset\.fsig/, "no inline stamp precedence remains");
  assert.match(exportSrc, /shared\.stampQuery\(/, "the classic page builds its stamped queries through the shared helper");
  const componentsSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.doesNotMatch(componentsSrc, /function attachmentCapability|function referenceAction|const revealQuery =/, "components.js keeps no copy of the rule");
  assert.match(componentsSrc, /from "\.\/shared\/capabilities\.js"/);
  console.log("seam (b) cases passed");
}

// Seam (c) (#48): one control protocol. The rule for who may be composed to, the words, the
// queries and the meaning of every server answer are shared; each shell keeps its markup.
{
  assert.deepEqual(composeCapability({ injectable: true }, false), { inject: false, resume: false, mode: null }, "unpaired: nothing");
  assert.deepEqual(composeCapability({ injectable: true, state: "growing", agent: "claude" }, true), { inject: true, resume: false, mode: "tmux" });
  assert.deepEqual(composeCapability({ state: "finished", agent: "codex" }, true), { inject: false, resume: true, mode: "resume" });
  assert.equal(composeCapability({ state: "finished", agent: "codex", projActive: true }, true).mode, null, "constraint 2: another live session in the project → neither");
  assert.equal(composeCapability({ state: "finished", agent: "qoderwork" }, true).mode, null, "resume is claude/codex only");
  assert.deepEqual(composeCopy("tmux", false, "S"), { target: "Inject into: S", placeholder: "Type a prompt — it is pasted into the live tmux pane and submitted", notice: "Runs in the LIVE agent with its permissions. “Grant & send” authorises this pane until it restarts.", button: "Grant & send", revoke: false });
  assert.equal(composeCopy("tmux", true).button, "Send to pane"); assert.equal(composeCopy("tmux", true).revoke, true);
  assert.equal(composeCopy("resume", false, "S").target, "Send to: S"); assert.equal(composeCopy("resume", false).button, "Send prompt"); assert.equal(composeCopy("resume", false).notice, "");
  assert.equal(sendQuery("a b"), "/api/send?target=a%20b"); assert.equal(consentQuery("s1"), "/api/consent?target=s1"); assert.equal(consentQuery("s1", "revoke"), "/api/consent?op=revoke&target=s1");
  assert.equal(grantOutcome({ code: "passcode-required" }).kind, "passcode-required"); assert.equal(grantOutcome({ code: "bad-passcode" }).tone, "err");
  assert.equal(grantOutcome({ code: "locked", error: "wait" }).message, "wait"); assert.equal(grantOutcome({ ok: true }).kind, "granted"); assert.equal(grantOutcome(null).kind, "error");
  assert.deepEqual(sendOutcome({ ok: true }, "resume"), { kind: "sent", tone: "ok", message: "sent — the session is resuming" });
  assert.equal(sendOutcome({ code: "no-consent" }, "tmux").kind, "no-consent"); assert.equal(sendOutcome({ error: "x" }, "tmux").message, "x");
  // The flow: an unconsented pane grants first — the passcode dance — then sends.
  const calls = [];
  const fakePost = answers => async (url, body) => { calls.push([url, body]); return answers.shift(); };
  let r = await runSend({ target: "s1", mode: "tmux", consented: false, prompt: "hi", post: fakePost([{ code: "passcode-required" }]) });
  assert.deepEqual([r.step, r.outcome.kind, r.consented], ["grant", "passcode-required", false]);
  assert.deepEqual(calls, [["/api/consent?target=s1", ""]], "the grant went first, with an empty passcode body");
  calls.length = 0;
  r = await runSend({ target: "s1", mode: "tmux", consented: false, prompt: "hi", passcode: "pw", post: fakePost([{ ok: true }, { ok: true }]) });
  assert.deepEqual([r.step, r.outcome.kind, r.consented], ["send", "sent", true]);
  assert.deepEqual(calls, [["/api/consent?target=s1", "pw"], ["/api/send?target=s1", "hi"]], "granted with the passcode in the BODY, then sent");
  calls.length = 0;
  r = await runSend({ target: "s1", mode: "resume", consented: false, prompt: "go", post: fakePost([{ ok: true }]) });
  assert.deepEqual(calls, [["/api/send?target=s1", "go"]], "resume never grants");
  r = await runSend({ target: "s1", mode: "tmux", consented: true, prompt: "hi", post: fakePost([{ code: "no-consent" }]) });
  assert.deepEqual([r.outcome.kind, r.consented], ["no-consent", false], "a lapsed grant re-offers itself");
  r = await runSend({ target: "s1", mode: "tmux", consented: true, prompt: "hi", post: async () => { throw new Error("down"); } });
  assert.equal(r.outcome.kind, "unreachable");
  assert.equal((await runRevoke({ target: "s1", post: async () => ({ ok: true }) })).kind, "revoked");
  // Code lines only: the shells' comments still describe the protocol in prose.
  const codeOf = src => src.split("\n").filter(l => !/^\s*(\/\/|\*|\/\*)/.test(l)).join("\n");
  for (const [name, path] of [["rail.html", "../../claude-monitor/src/rail.html"], ["shell.html", "../src/shell.html"], ["control-store.js", "../../claude-monitor/src/codex-ui/control-store.js"]]) {
    const src = codeOf(readFileSync(new URL(path, import.meta.url), "utf8"));
    assert.match(src, /runSend\(\{/, `${name} sends through the shared flow`);
    assert.match(src, /runRevoke\(\{/, `${name} revokes through the shared flow`);
    assert.doesNotMatch(src, /\.code\s*===?\s*"(passcode-required|bad-passcode|locked|no-consent)"/, `${name} reads no raw server code of its own — the outcomes are the protocol's`);
    assert.doesNotMatch(src, /\/api\/(send|consent)\?/, `${name} builds no /api query of its own`);
  }
  for (const [name, path] of [["rail.html", "../../claude-monitor/src/rail.html"], ["shell.html", "../src/shell.html"]]) {
    const src = codeOf(readFileSync(new URL(path, import.meta.url), "utf8"));
    assert.match(src, /composeCopy\(/, `${name} takes its words from the shared table`);
    assert.doesNotMatch(src, /"(Grant & send|Send to pane|Send prompt|Inject into: |Send to: )"/, `${name} keeps no compose wording of its own`);
  }
  const stateSrc2 = readFileSync(new URL("../../claude-monitor/src/codex-ui/state.js", import.meta.url), "utf8");
  assert.match(stateSrc2, /composeCapability\(row, controlState\.paired\)/, "the app shell's canInject/canResume are the shared rule");
  console.log("seam (c) cases passed");
}

// Every module the shells load must PARSE as an ES module. `node --check` reads a `.js` file as
// CommonJS and passed control-store.js with a stray brace (#48); Chrome then refused the whole
// module graph and the app shell rendered blank. Checking each file as `.mjs` is the honest parse.
{
  const dirs = [new URL("../../claude-monitor/src/codex-ui/", import.meta.url), new URL("../../claude-replay-html/src/html/shared/", import.meta.url)];
  const tmp = mkdtempSync(join(tmpdir(), "ui-contract-parse-"));
  let checked = 0;
  try {
    for (const dir of dirs) {
      for (const name of readdirSync(dir).filter(n => n.endsWith(".js"))) {
        const copy = join(tmp, name.replace(/\.js$/, ".mjs"));
        writeFileSync(copy, readFileSync(new URL(name, dir), "utf8"));
        const result = spawnSync(process.execPath, ["--check", copy], { encoding: "utf8" });
        assert.equal(result.status, 0, `${name} does not parse as an ES module:\n${result.stderr}`);
        checked++;
      }
    }
  } finally { rmSync(tmp, { recursive: true, force: true }); }
  assert.ok(checked >= 15, `parsed ${checked} modules`);
  console.log(`module parse cases passed (${checked} modules)`);
}

// Seam (e) (#49): one two-zone reducer. Both clients apply the plan it returns; neither keeps
// cursor arithmetic of its own.
{
  assert.equal(cursorText(freshCursor()), "0.0.0.0", "a fresh cursor's epoch 0 resyncs on the first pull");
  assert.equal(pullQuery("s 1", { epoch: 2, committed: 5, gen: 1, index: 3 }), "session=s%201&cursor=2.5.1.3");
  assert.equal(recordsQuery("s1", { offset: 10, len: 20 }, 2), "session=s1&from=10&len=20&epoch=2");
  assert.deepEqual(parseRecords('{"a":1}\n\n{"b":2}\n'), [{ a: 1 }, { b: 2 }]);
  const idle = reducePull({ epoch: 2, committed: 5, gen: 1, index: 0 }, 5, { epoch: 2, committed: [], committed_from: 5, provisional_gen: 1, provisional_from: 0, provisional: [] });
  assert.equal(idle.idle, true); assert.deepEqual(idle.steps, []); assert.equal(idle.changedFrom, Infinity); assert.equal(idle.length, 5);
  const resync = reducePull(freshCursor(), 0, { epoch: 1, committed: [{ id: "a" }, { id: "b" }], committed_from: 0, provisional_gen: 0, provisional_from: 0, provisional: [{ id: "p" }] });
  assert.equal(resync.resync, true); assert.equal(resync.changedFrom, 0);
  assert.deepEqual(resync.steps.map(s => s.op), ["truncate", "truncate", "append", "truncate", "append"], "resync, then commit, then provisional");
  assert.deepEqual(resync.cursor, { epoch: 1, committed: 2, gen: 0, index: 1 }); assert.equal(resync.length, 3);
  const extend = reducePull(resync.cursor, resync.length, { epoch: 1, committed: [], committed_from: 2, provisional_gen: 0, provisional_from: 1, provisional: [{ id: "q" }] });
  assert.equal(extend.idle, false); assert.deepEqual(extend.steps, [{ op: "truncate", to: 3 }, { op: "append", records: [{ id: "q" }] }], "a same-gen append keeps the provisional prefix");
  assert.equal(extend.changedFrom, 3); assert.deepEqual(extend.cursor, { epoch: 1, committed: 2, gen: 0, index: 2 }); assert.equal(extend.length, 4);
  const replace = reducePull(extend.cursor, extend.length, { epoch: 1, committed: [], committed_from: 2, provisional_gen: 1, provisional_from: 0, provisional: [{ id: "r" }] });
  assert.deepEqual(replace.steps, [{ op: "truncate", to: 2 }, { op: "append", records: [{ id: "r" }] }], "a gen bump replaces the provisional zone");
  assert.equal(replace.changedFrom, 2); assert.deepEqual(replace.cursor, { epoch: 1, committed: 2, gen: 1, index: 1 });
  const commit = reducePull(replace.cursor, replace.length, { epoch: 1, committed: [{ id: "c" }], committed_from: 2, provisional_gen: 2, provisional_from: 0, provisional: [] });
  assert.deepEqual(commit.steps, [{ op: "truncate", to: 2 }, { op: "append", records: [{ id: "c" }] }], "a commit lands at committed_from and empties the provisional zone (already at the prefix)");
  assert.equal(commit.changedFrom, 2); assert.deepEqual(commit.cursor, { epoch: 1, committed: 3, gen: 2, index: 0 }); assert.equal(commit.length, 3);
  const shrink = reducePull({ epoch: 1, committed: 3, gen: 2, index: 2 }, 5, { epoch: 1, committed: [], committed_from: 3, provisional_gen: 2, provisional_from: 1, provisional: [] });
  assert.deepEqual(shrink.steps, [{ op: "truncate", to: 4 }], "a shorter provisional zone truncates even with nothing to append");
  // `idle` is the classic page's early-return rule (both zones empty); `changedFrom` is what a
  // store must repaint. Deliberately different questions: this reply is idle AND changes the
  // store — the app shell's update gate reads changedFrom, never idle.
  assert.equal(shrink.idle, true); assert.equal(shrink.changedFrom, 4);
  const storeSrc0 = readFileSync(new URL("../../claude-monitor/src/codex-ui/record-store.js", import.meta.url), "utf8");
  assert.match(storeSrc0, /if \(plan\.changedFrom !== Infinity \|\| reply\.meta\) this\.handlers\.update/, "the store repaints on changedFrom, not on the idle rule");
  const bump = reducePull({ epoch: 1, committed: 3, gen: 2, index: 0 }, 3, { epoch: 2, committed: [{ id: "x" }], committed_from: 0, provisional_gen: 0, provisional_from: 0, provisional: [] });
  assert.equal(bump.resync, true); assert.equal(bump.changedFrom, 0); assert.deepEqual(bump.cursor, { epoch: 2, committed: 1, gen: 0, index: 0 });
  const exportSrc2 = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(exportSrc2, /shared\.reducePull\(pc, records\.length, r\)/, "the classic page applies the shared plan");
  assert.doesNotMatch(exportSrc2, /pc\.committed \+ r\.provisional_from|pc\.committed\+\+/, "…and keeps no cursor arithmetic of its own");
  assert.match(exportSrc2, /shared\.pullQuery\(|shared\.recordsQuery\(|shared\.parseRecords\(/, "…nor its own queries");
  const storeSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/record-store.js", import.meta.url), "utf8");
  assert.match(storeSrc, /reducePull\(this\.cursor, this\.records\.length, reply\)/, "the app shell's store applies the shared plan");
  assert.doesNotMatch(storeSrc, /c\.committed \+ reply\.provisional_from|c\.committed\+\+/, "…and keeps no cursor arithmetic of its own");
  console.log("seam (e) cases passed");
}

// #52: the pane follows the transcript. The rule for "the current turn" is one DOM-free
// function shared by the outline's focus and the `]`/`[` stepping.
{
  const units = [
    { key: "u0", type: "user" }, { key: "p0", type: "process" }, { key: "a0", type: "assistant" },
    { key: "u1", type: "user" }, { key: "a1", type: "assistant" },
    { key: "u2", type: "user" }, { key: "a2", type: "assistant" }
  ];
  assert.equal(currentTurnIndex(units, "u0"), 0, "at the first turn");
  assert.equal(currentTurnIndex(units, "a0"), 0, "inside the first turn's reply: still the first turn");
  assert.equal(currentTurnIndex(units, "u1"), 1);
  assert.equal(currentTurnIndex(units, "a2"), 2, "at the tail: the last turn");
  assert.equal(currentTurnIndex(units, null), -1, "nothing at the top yet");
  assert.equal(currentTurnIndex(units, "nope"), -1, "an unknown unit names no turn");
  assert.equal(currentTurnIndex([{ key: "p", type: "process" }], "p"), -1, "no user turn at or before");
  assert.match(appSource, /afterScroll: \(\) => \{ updateStickyHeaders\(\); updateOutlineFocus\(\); \}/, "the spy runs on every scroll");
  assert.match(appSource, /row\.classList\.toggle\("current", on\)/, "the current row carries the reference CSS's `current` class");
  assert.match(appSource, /row\.setAttribute\("aria-current", "true"\)/, "…and aria-current");
  assert.match(appSource, /return currentTurnIndex\(recordState\.units, unitAtTop\(\)\);/, "the keys step from the same rule");
  assert.match(appSource, /pane\.contains\(transcript\)\) return;/, "the reveal never scrolls the transcript");
  console.log("#52 outline focus cases passed");
}

// #72: a feed error is a spell, not a verdict — the classic page keeps polling and clears
// the notice on the first feed reply after it.
{
  const src = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(src, /if \(reply\.t === "error"\) \{ showFatal\(reply\.message\); return null; \}/, "an error reply shows the notice and returns");
  assert.doesNotMatch(src, /reply\.t === "error"\) \{ clearInterval/, "…without stopping the feed");
  assert.match(src, /var clearFatal = function \(\) \{/, "the notice can be cleared");
  assert.match(src, /return null; \} \/\/ transient: keep polling\n\s*clearFatal\(\);/, "…and is, on the next feed reply");
  assert.match(src, /if \(reply\.t === "redirect"\) \{ clearInterval\(pullTimer\);/, "a hand-off still stops this feed: it is a navigation");
  console.log("#72 transient feed error cases passed");
}

// #71: the classic page's hit count follows the records — counted on arrival, dropped on a
// tail rewrite, painted by one painter that keeps a stepping reader's "k/N" form.
{
  const src = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(src, /countNewRecord\(records\.length - 1\);\n  \}/, "pushRecord counts an arriving record");
  assert.match(src, /dropHitsFrom\(from\);\n    records\.length = from;/, "resetFrom drops the hits a rewrite takes");
  assert.match(src, /function paintQCount\(\) \{/, "one painter for the count");
  assert.doesNotMatch(src, /\$\("qcount"\)\.textContent =\n\s*\(hr\.start/, "stepHit paints through it too");
  console.log("#71 hit-count cases passed");
}

// #64: the app shell's jump control carries the classic page's "N new messages" count as text.
{
  assert.match(appSource, /count\.textContent = n \? `\$\{n\} new message\$\{n === 1 \? "" : "s"\}` : "";/, "the pill says how many");
  assert.match(appSource, /button\.classList\.toggle\("has-new", n > 0\)/, "…and widens only when there is something to say");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.jump-to-bottom\.has-new\{/, "the pill's shape is production chrome, not the generated reference");
  console.log("#64 new-messages pill cases passed");
}

// #65: the app shell's queue renderer prints the queued prompt's text, not only a label.
{
  const src = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(src, /const text = view\.html \? `<div class="renderer-queue-text">\$\{view\.html\}<\/div>` : `<small>no text recorded<\/small>`;/, "the queued text is rendered from the record's body");
  assert.doesNotMatch(src, /<strong>\$\{escapeText\(view\.summary \|\| "Queued input"\)\}<\/strong><small>queued input<\/small>/, "the bare label is gone");
  console.log("#65 queued text cases passed");
}

// #66: the classic lightbox's "Reveal in file manager" runs the caller's STAMPED reveal; it
// never builds an unstamped __reveal of its own.
{
  const src = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(src, /function fileview\(path, imgUrl, text, reveal\) \{/, "the lightbox takes the reveal action");
  assert.doesNotMatch(src, /"__reveal\?path=" \+ encodeURIComponent\(path\)/, "…and builds no unstamped request");
  assert.match(src, /fileview\(path, URL\.createObjectURL\(b\), null, fallback\)/, "the image view gets it from openArtifact");
  assert.match(src, /fileview\(path, null, t, fallback\)/, "…and so does the text view");
  assert.equal((src.match(/var reveal = function \(\) \{\s*return fetch\("__reveal\?" \+ shared\.stampQuery/g) || []).length, 2, "both offered-path sites return their stamped request, so the caption can report it");
  console.log("#66 lightbox reveal cases passed");
}

// #62: the runtime snapshot's rows are phrased once — "unknown" is a recorded fact not yet
// seen, "not recorded by <agent>" a fact the format has no room for — and both panes use it.
{
  const claude = runtimeRows({ effort: "xhigh", permission: null, client: "2.1.234", recorded: ["effort", "permission", "client"] });
  const by = Object.fromEntries(claude.map(r => [r.key, r]));
  assert.equal(by.effort.state, "value"); assert.equal(by.effort.value, "xhigh");
  assert.equal(by.permission.state, "unknown", "recorded by Claude Code, not seen yet");
  assert.equal(by.sandbox.state, "absent", "Claude Code has no sandbox");
  assert.equal(by.context.state, "absent");
  assert.equal(runtimeText(by.permission, "Claude Code"), "unknown");
  assert.equal(runtimeText(by.sandbox, "Claude Code"), "not recorded by Claude Code");
  assert.equal(runtimeText(by.sandbox), "not recorded by this agent");
  const codex = Object.fromEntries(runtimeRows({ context_left: 75, sandbox: "workspace-write", recorded: ["context", "effort", "sandbox"] }).map(r => [r.key, r]));
  assert.equal(codex.context.value, "75% left"); assert.equal(codex.effort.state, "unknown"); assert.equal(codex.client.state, "absent");
  assert.deepEqual(runtimeRows(null).map(r => r.state), runtimeRows(null).map(() => "absent"), "no snapshot: nothing declared");
  assert.deepEqual(RUNTIME_ALWAYS, ["context", "effort", "sandbox", "permission"]);
  assert.match(appSource, /runtimeRows\(usage\.runtime\)\.filter\(r => r\.state !== "absent" \|\| RUNTIME_ALWAYS\.includes\(r\.key\)\)\.map\(r => \[r\.label, runtimeText\(r, agentName\(row\.agent\)\)\]\)/, "the app shell's Runtime group is the shared rows");
  assert.doesNotMatch(appSource, /not provided by this protocol/, "the old wording is gone");
  const exportSrc = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(exportSrc, /shared\.runtimeRows\(rt\)\.forEach\(function \(r\) \{ if \(r\.state === "unknown"\) rrow\(r\.label, "unknown"\); \}\);/, "the classic panel says unknown through the same helper");
  console.log("#62 runtime wording cases passed");
}

// #50: the header shows the session id short — one shortener for every page — and a click
// copies the transcript path with the classic page's wording.
{
  assert.equal(snipId("530339ac-689c-4399-bef8-fd9f64101558"), "530339ac");
  assert.equal(snipId("rollout-2026-08-09T12-00-00-019b2c4e-1111-4222-8333-444455556666"), "019b2c4e");
  assert.equal(snipId("short-id"), "short-id");
  assert.equal(snipId("a-long-opaque-identifier"), "a-long-o");
  assert.equal(snipId(null), "");
  assert.match(appSource, /const sessionIdChip = document\.createElement\("button"\);/, "the chip is runtime chrome");
  assert.match(appSource, /sessionIdChip\.textContent = snipId\(sid\);/, "…showing the shared short form");
  assert.match(appSource, /flash\(copied \? "copied transcript path" : "copy blocked — ⌘C the path"\);/, "…with the classic page's wording");
  const exportSrc = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(exportSrc, /var snipId = shared\.snipId;/, "the classic page reads the same shortener");
  assert.doesNotMatch(exportSrc, /function snipId\(/, "…and keeps no copy of its own");
  console.log("#50 session id chip cases passed");
}

// #54: the sidebar collapses into its icon rail by key from anywhere, and the rail's buttons
// all lead somewhere.
{
  const ev = (key, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...extra });
  assert.equal(resolveKey(ev("\\"), "view").action, "sidebar-toggle");
  assert.equal(resolveKey(ev("\\"), "list").action, "sidebar-toggle", "from the list too");
  assert.equal(resolveKey(ev("\\"), "view", { tagName: "INPUT" }), null, "never while typing");
  assert.match(appSource, /"sidebar-toggle": \(\) => toggleSidebar\(!indexState\.sidebarOpen\)/, "the shell acts on it");
  assert.match(appSource, /byId\("sidebarMiniWrite"\)\.onclick = \(\) => byId\("writeSwitch"\)\.click\(\);/, "the rail's write button reaches the switch");
  assert.match(appSource, /byId\("sidebarMiniSearch"\)\.onclick = openGlobalSearch;/, "…its search button the global search");
  assert.match(appSource, /byId\("sidebarMiniAttention"\)\.onclick = \(\) => byId\("attentionBtn"\)\.click\(\);/, "…its attention button the filter");
  assert.match(appSource, /hintFor\("sidebar-toggle"\)/, "the key is discoverable on the control");
  console.log("#54 sidebar rail cases passed");
}

// #55: the outline pane's third state — hidden outright by key from anywhere, the transcript
// taking the whole remaining width; the header's toggle brings it back.
{
  const ev = (key, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...extra });
  assert.equal(resolveKey(ev("o"), "view").action, "navigator-toggle");
  assert.equal(resolveKey(ev("o"), "list").action, "navigator-toggle", "from the list too");
  assert.equal(resolveKey(ev("o"), "view", { tagName: "TEXTAREA" }), null, "never while typing");
  assert.match(appSource, /"navigator-toggle": \(\) => setNavigatorHidden\(!uiState\.navigatorHidden\)/, "the key hides and shows");
  assert.match(appSource, /function setNavigatorHidden\(hidden\) \{ uiState\.navigatorHidden = hidden; persist\(\); renderNavigator\(\); viewport\.remeasure\(\); \}/, "one path, remembered, re-measured");
  assert.match(appSource, /uiState\.navigatorHidden \? setNavigatorHidden\(false\) : toggleNavigator\(!uiState\.navigatorOpen\)/, "the header's toggle brings a hidden pane back");
  assert.match(appSource, /if \(open\) uiState\.navigatorHidden = false;/, "opening the pane un-hides it");
  assert.match(appSource, /hintFor\("navigator-toggle"\)/, "the key is discoverable on the controls");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.workspace\.navigator-hidden \.session-main\{grid-template-columns:0 minmax\(0,1fr\)\}/, "hidden: the transcript has the whole width");
  assert.match(css, /\.workspace\.navigator-hidden \.session-navigator\{display:none\}/, "…and no rail remains");
  const stateSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/state.js", import.meta.url), "utf8");
  assert.match(stateSrc, /navigatorHidden: localStorage\.getItem\("am-prod-navigator-hidden"\) === "1"/, "remembered per viewer");
  console.log("#55 outline pane cases passed");
}

// #56: the tasks pane's order — by group, then by id — as a pure function.
{
  const tasks = [
    { id: "10", status: "pending" }, { id: "2", status: "completed" }, { id: "u4", status: "pending" },
    { id: "7", status: "in_progress" }, { id: "1", status: "completed" }, { id: "3", status: "pending" },
    { id: "zeta", status: "pending" }, { id: "9", status: "parked" }, { id: "u1", status: "completed" },
  ];
  assert.deepEqual(taskOrder(tasks).map(r => r.task.id), ["1", "2", "u1", "7", "3", "10", "u4", "zeta", "9"], "completed, running, pending, other; numbers as numbers; plain before u; text after");
  assert.deepEqual(taskGroups(tasks).map(g => [g.key, g.rows.length]), [["completed", 3], ["in_progress", 1], ["pending", 4], ["other", 1]]);
  assert.deepEqual(taskOrder(tasks).map(r => r.index).slice(0, 2), [4, 1], "each row keeps the stream's index for its record target");
  assert.equal(taskGroupKey("Done"), "completed"); assert.equal(taskGroupKey("in-progress"), "in_progress"); assert.equal(taskGroupKey(undefined), "pending"); assert.equal(taskGroupKey("weird"), "other");
  const same = [{ id: "x", status: "pending" }, { id: "x", status: "pending" }];
  assert.deepEqual(taskOrder(same).map(r => r.index), [0, 1], "ties keep the stream's order");
  assert.deepEqual(taskGroups([]), [], "no tasks, no groups");
  assert.match(appSource, /taskGroups\(tasks\)\.map\(group => `<div class="work-group" data-task-group="\$\{group\.key\}">/, "the pane renders the groups with a boundary");
  console.log("#56 task order cases passed");
}

// #58 / #59: the outline's lists are bounded scrollers of their own; the focused turn is kept in
// view through the pane's scroller, never the window.
{
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.session-navigator>\.outline-card\.open>\.outline-card-body>\.navigator-list\{flex:1 1 auto;min-height:0;overflow-y:auto;/, "each open card's list scrolls itself");
  assert.match(css, /\.session-navigator>\.outline-card\.open\{flex:1 1 auto;min-height:96px\}/, "open cards share the pane's height down to a floor");
  assert.match(appSource, /function revealInPane\(/, "the focused turn is revealed in the pane");
  assert.match(appSource, /revealInPane\(row\)/, "…through the pane's own scroller, never the window");
  console.log("#58/#59 pane scroller cases passed");
}

// #57: where the tasks pane centers — the running run's middle, else the done/pending boundary.
{
  const rows = statuses => taskOrder(statuses.map((status, i) => ({ id: String(i + 1), status })));
  assert.deepEqual(taskCenterTarget(rows(["completed", "completed", "in_progress", "in_progress", "in_progress", "pending"])), { index: 3, edge: "middle", why: "running" });
  assert.deepEqual(taskCenterTarget(rows(["completed", "in_progress", "pending", "pending"])), { index: 1, edge: "middle", why: "running" }, "one running row is its own middle");
  assert.deepEqual(taskCenterTarget(rows(["completed", "completed", "pending", "pending", "pending"])), { index: 2, edge: "top", why: "boundary" }, "none running: the first pending row's top is the boundary");
  assert.deepEqual(taskCenterTarget(rows(["completed", "completed"])), { index: 1, edge: "bottom", why: "all-done" });
  assert.deepEqual(taskCenterTarget(rows(["pending", "pending"])), { index: 0, edge: "top", why: "all-pending" });
  assert.equal(taskCenterTarget([]), null);
  const ev = (key, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...extra });
  assert.equal(resolveKey(ev("c"), "view").action, "tasks-center");
  assert.equal(resolveKey(ev("c"), "list"), null, "a view key");
  assert.match(appSource, /"tasks-center": \(\) => centerTasks\(\)/, "the shell acts on it");
  assert.match(appSource, /pane\.scrollTop \+= edge - \(p\.top \+ p\.height \/ 2\);/, "the pane's own scroller moves");
  assert.match(appSource, /!node\.contains\(transcript\)/, "…never the transcript's");
  assert.match(appSource, /hintFor\("tasks-center"\)/, "the key is discoverable on the control");
  console.log("#57 tasks center cases passed");
}

// #60: a task's details, as the popover shows them — DOM-free.
{
  const d = taskDetails({ id: "52", subject: "Fix the parser", description: "First paragraph\nwraps here.\n\nSecond paragraph.", active_form: "Fixing the parser", status: "in_progress", blocked_by: ["54"], blocks: ["57", "58"] }, 41);
  assert.equal(d.subject, "Fix the parser"); assert.equal(d.label, "Running"); assert.equal(d.status, "in_progress");
  assert.deepEqual(d.paragraphs, ["First paragraph wraps here.", "Second paragraph."], "blank lines split paragraphs, single newlines join");
  assert.equal(d.activeForm, "Fixing the parser"); assert.deepEqual(d.blockedBy, ["54"]); assert.deepEqual(d.blocks, ["57", "58"]); assert.equal(d.target, 41);
  const bare = taskDetails({ id: "7", status: "completed" }, null);
  assert.equal(bare.subject, "Task 7"); assert.equal(bare.label, "Completed"); assert.deepEqual(bare.paragraphs, []); assert.equal(bare.target, null);
  assert.equal(taskDetails({ id: "1", status: "parked" }).label, "parked", "an unknown status shows as itself");
  assert.match(appSource, /data-task-open="\$\{index\}" title="Task details" aria-haspopup="dialog"/, "a row opens the details");
  assert.doesNotMatch(appSource, /class="work-task-head" \$\{nav\}/, "…and no longer jumps by itself");
  assert.match(appSource, /class="task-popover-jump" data-task-record="\$\{d\.target\}"/, "the jump is an action inside the popover");
  assert.match(appSource, /if \(event\.key === "Escape" && !taskPopover\.hidden\)/, "Escape dismisses it");
  assert.match(appSource, /if \(restoreFocus && taskPopoverOpener\?\.isConnected\) taskPopoverOpener\.focus\(\);/, "focus returns to the row");
  console.log("#60 task details cases passed");
}
