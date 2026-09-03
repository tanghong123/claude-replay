import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { RecordStore } from "../../claude-monitor/src/codex-ui/record-store.js";
import { promptShouldCollapse, rawTurnHtml, rendererStartsClosed } from "../../claude-monitor/src/codex-ui/components.js";
import { attachmentCapability, referenceAction, revealQuery, stampQuery } from "../../claude-replay-html/src/html/shared/capabilities.js";
import { DEFAULT_READING, READING_KEY, SIZE_MIN, clampSize, loadReading, parseReading, readingVars } from "../../claude-replay-html/src/html/shared/reading.js";
import { KEYMAP, hintFor, isEditable, resolveKey } from "../../claude-replay-html/src/html/shared/keymap.js";
import { agentRecordTargets, Projection, taskRecordTargets, taskStatus, viewRecord } from "../../claude-monitor/src/codex-ui/view-model.js";
import { revealNavigationContext } from "../../claude-monitor/src/codex-ui/viewport.js";
import { PREVIEW_CSP, sandboxDocument } from "../../claude-monitor/src/codex-ui/sandbox.js";
import { families, familyKey, groupSessions, groupVisible, hideAction, ignoreQuery, rowVisible, visibleTree } from "../../claude-replay-html/src/html/shared/session-visibility.js";
import { REASONS, denoteState, displayState, needsPerson, stateTip } from "../../claude-replay-html/src/html/shared/state-labels.js";
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
