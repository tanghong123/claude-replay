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
import { recordTextSize, LIVE_SEARCH_LIMIT, recordText, recordTextParts, parseScope, scopeLetters, activeLetters, scopeMask, stripTags, countOcc, wholeAt, directMask, CLASS_BIT } from "../../claude-monitor/src/codex-ui/shared/search.js";
import { fmtTime, fmtDur } from "../../claude-monitor/src/codex-ui/shared/time.js";
import { RESULT_MARK, resultBodyHtml } from "../../claude-replay-html/src/html/shared/parts.js";
import { isInteraction, interactionCard, interactionHtml } from "../../claude-replay-html/src/html/shared/interaction.js";
import { splitQuery, zeroCounts, countRecord, countLabel, writePrefix, CLASS_ORDER, MIN_NEEDLE } from "../../claude-replay-html/src/html/shared/search.js";
import { chainWalk } from "../../claude-replay-html/src/html/shared/filter.js";
import { prefixSums, indexAt, rangeForScroll, rangeAround, clampRange, padHeights, heightChanged, correction, firstVisible, classifyScroll } from "../../claude-replay-html/src/html/shared/virtual-window.js";
import { taskGlyph, taskStatus as cardStatus, taskStamp, taskDates, taskChips, taskRowMeta, taskSections, taskCardHtml } from "../../claude-replay-html/src/html/shared/task-card.js";
import { displayName, toolHead, stateLabel } from "../../claude-monitor/src/codex-ui/shared/tool-head.js";
import { DEFAULT_READING, READING_KEY, SIZE_MIN, clampSize, loadReading, parseReading, readingVars } from "../../claude-replay-html/src/html/shared/reading.js";
import { KEYMAP, hintFor, isEditable, resolveKey } from "../../claude-replay-html/src/html/shared/keymap.js";
import { agentRecordTargets, currentTurnIndex, Projection, taskRecordTargets, taskStatus, viewRecord, taskOrder, taskGroups, taskGroupKey, taskCenterTarget, taskDetails, artifactRoster, humanTokens, compactionTick } from "../../claude-monitor/src/codex-ui/view-model.js";
import { revealNavigationContext } from "../../claude-monitor/src/codex-ui/viewport.js";
import { PREVIEW_CSP, sandboxDocument } from "../../claude-monitor/src/codex-ui/sandbox.js";
import { families, familyKey, groupSessions, groupVisible, hideAction, ignoreQuery, rowVisible, visibleTree } from "../../claude-replay-html/src/html/shared/session-visibility.js";
import { REASONS, denoteState, displayState, needsPerson, stateTip } from "../../claude-replay-html/src/html/shared/state-labels.js";
import { composeCapability, composeCopy, consentQuery, grantOutcome, runRevoke, runSend, sendOutcome, sendQuery } from "../../claude-replay-html/src/html/shared/control-protocol.js";
import { cursorText, freshCursor, parseRecords, pullQuery, recordsQuery, reducePull } from "../../claude-replay-html/src/html/shared/record-stream.js";
import { applyViewChoices, parseViewMemory, serializeViewMemory, viewChoices, viewMemoryKey } from "../../claude-monitor/src/codex-ui/view-memory.js";

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
assert.match(productionCss, /\.session-parent\.is-live\{display:inline-flex;/, "the reference parent control is shown by a production state, not by editing the shell");
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
  assert.deepEqual(parseReading('{"size":"14.3","wrap":true,"wide":"yes"}'), { size: 14.5, wrap: true, wide: false, rawUser: false }, "size snaps to half steps; flags must be real booleans");
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
  assert.deepEqual(parseReading(null, { size: 12.5, wrap: true, wide: false }), { size: 12.5, wrap: true, wide: false, rawUser: false }, "a page's own defaults apply");
  assert.deepEqual(parseReading('{"size":14}', { size: 12.5, wrap: true, wide: false }), { size: 14, wrap: true, wide: false, rawUser: false }, "missing fields fall back to the defaults, not to false");
  assert.deepEqual(parseReading('{"size":14,"wrap":false}'), { size: 14, wrap: false, wide: false, rawUser: false });
  const store = new Map([["claude-replay-export-ms", "14"], ["claude-replay-export-wrap", "0"]]);
  const get = k => (store.has(k) ? store.get(k) : null), set = (k, v) => store.set(k, v), del = k => store.delete(k);
  const legacy = { size: "claude-replay-export-ms", wrap: "claude-replay-export-wrap", wide: "claude-replay-export-wide" };
  const migrated = loadReading(get, set, { size: 12.5, wrap: true, wide: false }, legacy, del);
  assert.deepEqual(migrated, { size: 14, wrap: false, wide: false, rawUser: false }, "the pre-#45 keys are folded in once");
  assert.equal(store.get(READING_KEY), JSON.stringify({ size: 14, wrap: false, wide: false, rawUser: false }), "…and written under the one key");
  assert.equal(store.has("claude-replay-export-ms"), false, "…and the legacy keys are removed");
  assert.deepEqual(loadReading(get, set, { size: 12.5, wrap: true, wide: false }, legacy, del), migrated, "the one key wins from then on");
  const empty = new Map();
  assert.deepEqual(loadReading(k => empty.get(k) ?? null, (k, v) => empty.set(k, v), { size: 12.5, wrap: true, wide: false }, legacy), { size: 12.5, wrap: true, wide: false, rawUser: false }, "nothing stored → the page's defaults");
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

// #50 / #83: the session id's short form is one shared function (the classic page's #sid reads
// it); the app shell shows no chip — its title menu carries the full id and the transcript path.
{
  assert.equal(snipId("530339ac-689c-4399-bef8-fd9f64101558"), "530339ac");
  assert.equal(snipId("rollout-2026-08-09T12-00-00-019b2c4e-1111-4222-8333-444455556666"), "019b2c4e");
  assert.equal(snipId("short-id"), "short-id");
  assert.equal(snipId("a-long-opaque-identifier"), "a-long-o");
  assert.equal(snipId(null), "");
  assert.doesNotMatch(appSource, /sessionIdChip|showSessionId/, "no chip in the header (#83)");
  assert.match(appSource, /data-session-copy-value="id"/, "the title menu carries the id");
  assert.match(appSource, /data-copy-session="path"/, "…and copies the transcript path");
  const exportSrc = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(exportSrc, /var snipId = shared\.snipId;/, "the classic page reads the shared shortener");
  console.log("#50/#83 session id cases passed");
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

// #58 / #59 / #74: plain outline panes — each list scrolls itself under a max-height, the
// column scrolls as a whole with the caption and the heads sticking in a stack, a head click
// toggles its own pane only, and nothing is shared between panes.
{
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.session-navigator\{display:flex;flex-direction:column;min-height:0;overflow-y:auto;/, "the column scrolls as a whole");
  assert.match(css, /\.session-navigator>\*\{flex:0 0 auto\}/, "every pane sits at its own height — nothing is shared");
  assert.match(css, /\.session-navigator>\.outline-caption\{position:sticky;top:0;/, "the caption sticks at the top");
  assert.match(css, /\.session-navigator>\.outline-card>\.outline-card-head\{position:relative;z-index:2;/, "the heads are shifted into a stack by app.js — not CSS-sticky, which cannot leave the pane");
  assert.doesNotMatch(css, /outline-card-head\{position:sticky/, "…and never stick to the bottom");
  assert.match(css, /\.navigator-list\{max-height:min\(48vh,560px\);overflow-y:auto;/, "a long list scrolls itself under a max-height");
  assert.doesNotMatch(css, /outline-card\.open\{flex:|\.focus\{flex:/, "no shared height, no focus share");
  assert.match(appSource, /function stackOutlineHeads\(\) \{/, "the stack offsets are measured");
  assert.match(appSource, /const shift = Math\.max\(0, slot - cardTop\);\n\s*if \(shift > 0\) head\.style\.transform = `translateY\(\$\{shift\}px\)`;\n\s*slot \+= head\.offsetHeight;/, "…each head shifted to its slot under the ones above it");
  assert.match(appSource, /byId\("sessionNavigator"\)\.addEventListener\("scroll", stackOutlineHeads, \{ passive: true \}\);/, "…on every scroll of the column");
  assert.match(appSource, /if \(card\) \{ const key = card\.dataset\.navCardToggle; uiState\.navCards\.has\(key\) \? uiState\.navCards\.delete\(key\) : uiState\.navCards\.add\(key\); persist\(\); renderNavigator\(\); return; \}/, "a head click toggles its own pane, whole head");
  assert.doesNotMatch(appSource, /navFocus|classList\.toggle\("focus"/, "no focus state");
  const stateSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/state.js", import.meta.url), "utf8");
  assert.doesNotMatch(stateSrc, /navFocus/, "…nor remembered");
  assert.match(appSource, /revealInPane\(row\)/, "the focused turn is revealed through the pane's own scroller");
  console.log("#58/#59/#74 outline pane cases passed");
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

// #61: the agents pane's row switches the view to the sub-agent; the spawn jump is secondary.
{
  assert.match(appSource, /<button class="outline-agent" type="button" data-child-outline="\$\{escapeText\(agent\.id\)\}" title="Open the sub-agent's transcript">/, "the row itself opens the child's transcript");
  assert.match(appSource, /<button class="outline-agent-spawn" type="button" data-agent-record="\$\{target\}"/, "…and the spawn point is a control of its own");
  assert.match(appSource, /const spawn = target == null \? "" : /, "shown only when the record stream kept the spawn point");
  console.log("#61 agents pane cases passed");
}

// #67 / #68 / #69: the info pane shows only what the shell does not, with the cached-read and
// compaction facts; the turns pane shows compactions as epoch ticks.
{
  assert.match(appSource, /group\("Session", \[\["status", displayState\(row\)\.label\], \["turns", turns\], \["children", agents\]\]\)/, "no title / agent / project rows — the header and the tree row show them");
  assert.match(appSource, /\["cache read", usage\.cache_read\], \.\.\.\(usage\.compacted \? \[\["compacted", usage\.compacted\]\] : \[\]\)/, "cached-read tokens and the compaction summary, when there is one");
  assert.match(appSource, /if \(record\.kind === "compaction"\) epochs\.push\(\{ at: i, tick: compactionTick\(record\.head \|\| \{\}\) \}\)/, "a compaction becomes an epoch tick from the record's facts");
  assert.match(appSource, /<button class="outline-epoch" type="button" data-turn-record="\$\{r\.at\}"/, "…that jumps to the compaction record");
  console.log("#67/#68/#69 info and turns pane cases passed");
}

// #75 / #76: the sidebar head's three controls carry a glyph and a name.
{
  assert.match(appSource, /\["themeBtn", "moon", "Toggle light and dark"\], \["collapseBtn", "collapseStack", "Collapse every group"\], \["sidebarCollapse", "sidebar", "Collapse the session list into its icon rail"\]/, "theme, collapse-all and the sidebar collapse are drawn and named");
  assert.match(appSource, /if \(!button\.querySelector\("svg"\)\) button\.innerHTML = svg\(icon\);/, "…a glyph where the shell left none");
  const iconsSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/icons.js", import.meta.url), "utf8");
  for (const name of ["moon", "collapseStack", "sidebar"]) assert.ok(iconsSrc.includes(name + ":'"), `the ${name} glyph exists — svg() would otherwise draw the info glyph`);
  console.log("#75/#76 sidebar head controls cases passed");
}

// #84: the records a session opens with are never "new"; only later growth counts.
{
  assert.match(appSource, /const opening = lastRecordCount < 0;/, "the first apply after a reset is the open");
  assert.match(appSource, /if \(!wasFollowing && delta && !opening\) recordState\.newRecords \+= delta;/, "…and is not counted");
  assert.match(appSource, /reset: \(\) => \{ lastRecordCount = -1;/, "a reset re-arms the open");
  console.log("#84 first-open cases passed");
}

// #82: the way back from a sub-agent is visible and named, known even when the child was opened
// first, and `u` goes up.
{
  const ev = (key, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...extra });
  assert.equal(resolveKey(ev("u"), "view").action, "parent");
  assert.match(appSource, /parentBtn\.classList\.remove\("compat-hidden"\);/, "the control is un-hidden — the reference's !important rule hid it");
  assert.match(appSource, /<span class="session-parent-label">Parent session<\/span>/, "…and says what it does");
  assert.match(appSource, /parentHints\.set\(child\.dataset\.childOutline, indexState\.selected\);/, "a switch from a parent is remembered");
  assert.match(appSource, /const parent = known \|\| \(hint \? \{ id: hint, title: indexState\.rows\.get\(hint\)\?\.name \|\| hint \} : null\);/, "…and used when the meta has no ancestry");
  assert.match(appSource, /"parent": \(\) => \{ if \(parentBtn\.dataset\.parent\) selectSession\(parentBtn\.dataset\.parent, true\); \}/, "the key goes up");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.session-parent\.is-live\{display:inline-flex;/, "shown as a labelled control");
  console.log("#82 parent control cases passed");
}

// #79: a stamped reveal opens any existing offered path; the refusal blames nothing but a gone path.
{
  const viewer = readFileSync(new URL("../../claude-monitor/src/codex-ui/attachment-viewer.js", import.meta.url), "utf8");
  assert.match(viewer, /"Nothing to reveal — the path is gone"/, "the message names the one remaining reason");
  assert.doesNotMatch(viewer, /outside what this monitor may reveal/, "…and no longer a containment that is gone");
  const serve = readFileSync(new URL("../../claude-replay-html/src/html_export/serve.rs", import.meta.url), "utf8");
  assert.match(serve, /fn revealable\(&self, want: &Path\) -> Option<PathBuf>/, "the stamped reveal's own rule");
  assert.match(serve, /if let Some\(real\) = live\.revealable\(path\)/, "…is what the route asks");
  console.log("#79 reveal cases passed");
}

// #80: an image attachment is collapsed, then a thumbnail, then the lightbox.
{
  const src = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(src, /state\.openImages\.has\(id\) \? state\.openImages\.delete\(id\) : state\.openImages\.add\(id\)/, "the expanded images are reader state (persisted with the other choices since #114)");
  assert.match(src, /if \(capability\.action === "image"\) \{/, "an image attachment has its own rendering");
  assert.match(src, /class="renderer-image-toggle" data-image-toggle="\$\{escapeText\(view\.id \|\| ""\)\}" aria-expanded="\$\{open\}"/, "…a toggle first");
  assert.match(src, /class="renderer-image-thumb" \$\{attrs\} title="Open \$\{escapeText\(name\)\} at full size"><img src="\$\{escapeText\(source\)\}"/, "…then a thumbnail that opens the lightbox");
  assert.match(src, /state\.openImages\.has\(id\) \? state\.openImages\.delete\(id\) : state\.openImages\.add\(id\); actions\.rerender\?\.\(\);/, "the toggle flips and re-renders");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.renderer-image-thumb img\{display:block;max-height:320px;/, "the thumbnail is bounded");
  console.log("#80 image attachment cases passed");
}

// #77: expand-all beside collapse-all.
{
  assert.match(appSource, /expandBtn\.onclick = \(\) => \{ indexState\.collapsed\.clear\(\); persist\(\); renderTree\(\); \};/, "expand-all clears every fold and remembers it");
  assert.match(appSource, /byId\("collapseBtn"\)\.insertAdjacentElement\("afterend", expandBtn\);/, "…beside collapse-all");
  assert.match(appSource, /expandBtn\.innerHTML = svg\("expandStack"\);/, "…with a glyph the sprite has");
  const iconsSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/icons.js", import.meta.url), "utf8");
  assert.ok(iconsSrc.includes("expandStack:'"), "the expandStack glyph exists");
  console.log("#77 expand-all cases passed");
}

// #78: the published-artifact roster — grouped by URL, republishes counted, first-seen order,
// nested blocks walked — and the always-present header control.
{
  const pub = (url, name, extra = {}) => ({ kind: "tool", head: { artifact: { url, name, ...extra } } });
  const records = [
    { kind: "user" },
    pub("https://a/1", "deck", { icon: "📊", desc: "first" }),
    { kind: "act", body: [{ p: "blocks", items: [pub("https://a/2", "notes")] }] },
    pub("https://a/1", "deck v2", { desc: "again" }),
    { kind: "assistant" },
  ];
  const rows = artifactRoster(records);
  assert.deepEqual(rows.map(r => [r.url, r.count, r.name, r.icon, r.desc, r.at]), [["https://a/1", 2, "deck v2", "📊", "again", 3], ["https://a/2", 1, "notes", "", "", 2]], "one row per URL, first-seen order, republishes counted, latest name kept, nested blocks walked, last publishing record kept");
  assert.deepEqual(artifactRoster([]), []);
  assert.match(appSource, /artifactsBtn\.textContent = rows\.length \? `Artifacts \(\$\{rows\.length\}\) ▾` : "Artifacts ▾";/, "the control is always present, counted when there is something");
  assert.match(appSource, /artifactsBtn\.classList\.toggle\("disabled", rows\.length === 0\);/, "…grayed when the session published nothing");
  assert.match(appSource, /<a href="\$\{escapeText\(r\.url\)\}" target="_blank" rel="noopener"/, "a row is a link in a new tab");
  assert.match(appSource, /viewport\.jumpToRecord\(Number\(jump\.dataset\.artifactRecord\), "artifact"\)/, "…with a jump to the publishing record");
  console.log("#78 artifact roster cases passed");
}

// #86 / #87: the tick is a glyph and "from → to" in the rows' type — no prose — from structured
// head fields; the Outline caption takes the page background.
{
  assert.equal(humanTokens(0), "0"); assert.equal(humanTokens(999), "999"); assert.equal(humanTokens(8617), "8.6K"); assert.equal(humanTokens(594718), "594.7K"); assert.equal(humanTokens(1_200_000), "1.20M");
  const auto = compactionTick({ compact_trigger: "auto", compact_pre: 594718, compact_post: 8617 });
  assert.deepEqual(auto, { trigger: "auto", glyph: "⟳", title: "Compacted automatically — the context filled", sizes: "594.7K → 8.6K" });
  const manual = compactionTick({ compact_trigger: "manual" });
  assert.equal(manual.glyph, "✂"); assert.equal(manual.sizes, "", "no sizes when the record has none");
  assert.equal(compactionTick({}).trigger, "auto", "an unknown trigger reads as automatic");
  assert.match(appSource, /<span class="outline-epoch-glyph" aria-hidden="true">\$\{r\.tick\.glyph\}<\/span>/, "the glyph");
  assert.match(appSource, /<span class="outline-epoch-sizes">\$\{escapeText\(r\.tick\.sizes\)\}<\/span>/, "…and the sizes");
  assert.doesNotMatch(appSource, /context compacted/, "…and no prose");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.outline-epoch\{[^}]*font-size:11\.5px;line-height:1\.4;/, "the rows' own type (the reference's final .outline-label size)");
  assert.match(css, /\.session-navigator>\.outline-caption\{position:sticky;top:0;z-index:3;background:var\(--bg\)\}/, "the Outline caption takes the page background (#87)");
  const modRs = readFileSync(new URL("../../claude-replay-html/src/html_export/mod.rs", import.meta.url), "utf8");
  assert.match(modRs, /head\.insert\("compact_trigger"\.into\(\), json!\(trigger\.as_str\(\)\)\);/, "the wire carries the trigger");
  console.log("#86/#87 compaction tick cases passed");
}

// #98: the reader's anchor is the first visible ROW, kept fresh for changes nobody asked for,
// and the scrollbar thumb owns the position while it is held.
{
  // The rules moved into the shared engine with #107 step 4; the app shell's viewport is its
  // consumer, so each pin now reads whichever file OWNS the rule.
  const src = readFileSync(new URL("../../claude-replay-html/src/html/shared/virtual-window.js", import.meta.url), "utf8");
  const vpSrc = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
  // The rule is the shared module's since #107; this page measures the rects and reads it.
  assert.match(src, /const row = firstVisible\(\[\.\.\.child\.element\.querySelectorAll\("\[data-block-index\]"\)\]\.map\(rects\), viewportTop, Infinity, 1, true\);/, "the anchor descends to the first visible row of the unit");
  assert.equal(firstVisible([{ index: 0, top: 900, bottom: 1000, height: 100 }], 0, 500, 1, false), null, "a unit below the viewport is no anchor — the scroll offset places the window");
  assert.match(src, /const row = item\.querySelector\(`\[data-block-index="\$\{anchor\.block\}"\]`\);/, "…and the restore puts that row back");
  assert.match(src, /measureMounted\(anchor = this\.readerAnchor\(\)\) \{/, "an observer-driven measure restores the KEPT anchor, not one captured after the move");
  assert.match(src, /return this\.anchor \|\| this\.captureDomAnchor\(\);/, "the kept anchor, else a fresh one");
  assert.match(src, /this\.anchor = null;\n    this\.afterScroll\(\);/, "a scroll invalidates the kept anchor…");
  assert.match(src, /this\.reconcile\(range\.lo, range\.hi, Infinity, false, anchor\);\n    this\.syncAnchor\(\);/, "…and the deferred window update re-reads it once per batch");
  assert.match(vpSrc, /export class Viewport extends VirtualWindow \{/, "the app shell's viewport IS the shared engine (#107)");
  assert.match(vpSrc, /frame: elementFrame\(scroller\),/, "…driving it through the element frame");
  assert.match(src, /frame\.on\("pointerdown", event => \{ if \(frame\.isScrollbarTarget\(event\)\) this\.beginDrag\(\); \}/, "a pointer that lands on the scroller itself is on its scrollbar — no coordinate test, overlay scrollbars sit inside the client box");
  assert.match(src, /for \(const type of \["pointerup", "pointercancel", "mouseup"\]\) addEventListener\(type, \(\) => this\.endDrag\(\)/, "…released anywhere");
  assert.match(src, /const anchor = this\.following \|\| this\.dragging \? null : this\.captureDomAnchor\(\);\n    const anchorIndex/, "while dragging the window is placed by the scroll offset and nothing corrects it");
  assert.match(src, /this\.observer\.observe\(child, \{ box: "border-box" \}\);/, "a unit's height is its border box — padding and border changes count");
  console.log("#98 reader anchor cases passed");
}

// #108: row caps are the shared module's, on both pages.
{
  const parts = readFileSync(new URL("../../claude-replay-html/src/html/shared/parts.js", import.meta.url), "utf8");
  assert.match(parts, /^export \{ MAX_BUFFER_LINES, RESULT_MARK, resultBodyHtml, capLabel, capSplit, preLines, toLineOf, numRowsHtml, diffRowsHtml, capKey, rememberCap, capOpenHas, hiddenLines \};\s*$/m, "the module's export line");
  const vm = readFileSync(new URL("../../claude-monitor/src/codex-ui/view-model.js", import.meta.url), "utf8");
  assert.match(vm, /import \{ capSplit, capLabel, preLines, toLineOf, numRowsHtml, diffRowsHtml, capOpenHas \} from "\.\/shared\/parts\.js";/, "the app shell imports the shared rules");
  assert.match(vm, /export function partsHtml\(parts = \[\], recordId = "", state = null\) \{/, "parts render with the record id and the reader state");
  assert.match(vm, /class="cap-more-btn" data-cap-more=/, "the expander control");
  assert.match(vm, /parts: \(record\.body \|\| \[\]\)\.filter\(p => p\.p !== "blocks"\),/, "a tool view keeps its raw parts for render-time caps");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /const capMore = event\.target\.closest\("\[data-cap-more\]"\);/, "the click reveals in place…");
  assert.match(comp, /rememberCap\(state\.capOpen, capMore\.dataset\.capRecord, capMore\.dataset\.capOrd, Number\(capMore\.dataset\.capLines\) \|\| 0\);/, "…and remembers a small expansion");
  assert.match(comp, /const html = view\.parts \? partsHtml\(view\.parts, view\.id, state\) : view\.html;/, "the body renders from parts with the state");
  const vp = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
  assert.match(vp, /if \(target\?\.id && state\.capOpen\) state\.capOpen\.add\(`\$\{target\.id\}:\*`\);/, "a navigated-to record opens every cap");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.cap-more\{display:none\}\.cap-more\.shown\{display:block\}/, "hidden rows wait");
  assert.match(css, /\.renderer \.codebox \.lines\{max-height:none\}/, "the 360px scroll box is gone");
  console.log("#108 row cap cases passed");
}

// #109: the raw TEXT of a user turn, per turn and globally, one preference with the classic page.
{
  assert.deepEqual(parseReading('{"rawUser":true}'), { size: 12, wrap: false, wide: false, rawUser: true }, "the preference parses");
  const store = new Map([["claude-replay-export-rawuser", "1"]]);
  const migrated = loadReading(k => store.get(k) ?? null, (k, v) => store.set(k, v), { size: 12.5, wrap: true, wide: false, rawUser: false }, { size: "a", wrap: "b", wide: "c", rawUser: "claude-replay-export-rawuser" }, k => store.delete(k));
  assert.equal(migrated.rawUser, true, "the classic page's old key folds in once");
  assert.ok(!store.has("claude-replay-export-rawuser") && store.has(READING_KEY), "…and is removed");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /export const rawTextHtml = record => typeof record\?\.src === "string"/, "a user turn's raw view is its src");
  assert.match(comp, /return unit\.type === "user" && !!state\.rawUser;/, "…globally for user turns");
  assert.match(comp, /state\.rawTurns\.set\(key, rawToggle\.getAttribute\("aria-pressed"\) !== "true"\);/, "a per-turn override flips away from what the turn shows");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /data-reading-toggle="rawUser"/, "the global switch sits with the reading controls");
  assert.match(app, /if \(rawChanged && recordState\.records\.length\) viewport\.render\(\);/, "…and re-renders the mounted turns");
  console.log("#109 raw text cases passed");
}

// #110: the tool filter hides non-matching records, keeps turns as landmarks, opens the hits.
{
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /function filterChain\(record, wanted, hits, direct\) \{/, "a hit chain: the record or a nested one carries a selected tool");
  assert.match(app, /for \(const id of hits\) recordState\.folds\.set\(id, false\);/, "every record on a chain opens");
  assert.match(app, /recordState\.filterSnapshot = \{ folds: new Map\(recordState\.folds\)/, "the fold state is snapshotted for the clear");
  assert.match(app, /if \(target != null\) viewport\.jumpToRecord\(target, "filter"\);/, "…and the view lands on the nearest hit");
  assert.match(app, /answer\.classList\.add\("filter-hidden"\)/, "assistant answers hide");
  assert.match(app, /event\.classList\.toggle\("filter-hidden", !hit\);/, "non-matching rows hide inside their process");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.filter-hidden\{display:none!important\}/, "hidden is hidden");
  console.log("#110 tool filter cases passed");
}

// #111: the haystack is the record's text, shared; every hit in the window is marked.
{
  const record = { kind: "assistant", id: "b1", head: { name: "Bash", target: "echo hi" }, body: [{ p: "md", h: "<p>the <b>needle</b> &amp; more</p>" }, { p: "blocks", items: [{ kind: "bash", head: {}, body: [{ p: "pre", x: "needle in output" }] }] }] };
  assert.equal(recordText(record, stripTags).replace(/[ \t]+/g, " ").replace(/ ?\n ?/g, "\n").trim(), "Bash\necho hi\nthe needle & more\nneedle in output", "heads, stripped markdown and nested parts; never JSON field names");
  assert.equal(countOcc(recordText(record, stripTags).toLowerCase(), "needle", false), 2);
  assert.equal(countOcc("needles needle", "needle", true), 1, "whole words");
  assert.ok(wholeAt("a needle b", 2, 6) && !wholeAt("needles", 0, 6));
  assert.equal(directMask("bash"), CLASS_BIT.o | CLASS_BIT.b);
  const vm = readFileSync(new URL("../../claude-monitor/src/codex-ui/view-model.js", import.meta.url), "utf8");
  assert.match(vm, /export const plainText = record => recordText\(record, stripTags\)/, "the app shell searches the record's text");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /if \(!Number\.isInteger\(index\) \|\| !matched\.has\(index\) \|\| root\.parentElement\?\.closest\("\[data-block-index\]"\)\) continue;/, "every matched top-level record in the window is marked");
  assert.match(app, /mark\.className = "search-mark" \+ \(first \? " current" : ""\);/, "…the current record's first mark stronger");
  console.log("#111 search haystack cases passed");
}

// #112: times, one rule for both pages.
{
  const now = new Date(2026, 8, 4, 15, 0, 0);
  const today = new Date(2026, 8, 4, 9, 41, 0).getTime() / 1000;
  const thisYear = new Date(2026, 2, 9, 10, 20, 0).getTime() / 1000;
  const otherYear = new Date(2025, 2, 9, 10, 20, 0).getTime() / 1000;
  assert.match(fmtTime(today, now), /^\d{1,2}:\d{2}/, "today: a bare clock time");
  assert.ok(!/2026/.test(fmtTime(today, now)), "…without a date");
  assert.ok(/Mar/.test(fmtTime(thisYear, now)) && !/2026/.test(fmtTime(thisYear, now)), "this year: month and day, no year");
  assert.ok(/2025/.test(fmtTime(otherYear, now)), "another year: the year too");
  assert.equal(fmtDur(3725), "1h 2m"); assert.equal(fmtDur(90), "2m"); assert.equal(fmtDur(0), "");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /import \{ fmtTime \} from "\.\/shared\/time\.js";/, "the app shell imports the rule");
  assert.match(comp, /\$\{turnTime\(unit\)\}\$\{spot\}\$\{rawToggle\}/, "…and shows it beside the user bubble");
  console.log("#112 time cases passed");
}

// #113: a slash command is a turn card and a turns-pane row.
{
  const vm = readFileSync(new URL("../../claude-monitor/src/codex-ui/view-model.js", import.meta.url), "utf8");
  assert.match(vm, /if \(record\.kind === "user" \|\| record\.kind === "command"\) \{/, "a command starts a turn unit");
  assert.match(vm, /if \(record\.kind === "command"\) return \{ t: "user", id: record\.id/, "…as a user view with the command's badge and preview");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /class="turn user command" data-kind="user"/, "the card is the user's turn for filters and the spy");
  assert.match(comp, /class="command-head" type="button" data-prompt-toggle=/, "…folded until opened, through the prompt toggle");
  assert.match(comp, /<span class="command-badge">\/\$\{escapeText\(cmd\.name\)\}<\/span><span class="command-preview">/, "badge and preview");
  console.log("#113 command turn cases passed");
}

// #114: the reader's choices ride with the position and come back.
{
  const state = { folds: new Map([["b3", false]]), processFolds: new Map(), processExpanded: new Set(["process:b2"]), promptExpanded: new Set(), rawTurns: new Map([["user:t1", true]]), capOpen: new Set(["b5:0"]), openImages: new Set(["b9"]) };
  const choices = viewChoices(state);
  assert.deepEqual(choices, { folds: { b3: false }, processExpanded: ["process:b2"], rawTurns: { "user:t1": true }, capOpen: ["b5:0"], openImages: ["b9"] }, "Maps as objects, Sets as arrays, empties omitted");
  const round = parseViewMemory(serializeViewMemory({ following: false, key: "user:t1", top: 12.4, view: choices }));
  assert.deepEqual(round, { following: false, key: "user:t1", top: 12, view: choices }, "the choices survive the round trip");
  assert.deepEqual(parseViewMemory('{"following":true}'), { following: true }, "an old memory without choices still parses, without a view field");
  const fresh = { folds: new Map(), processFolds: new Map(), processExpanded: new Set(), promptExpanded: new Set(), rawTurns: new Map(), capOpen: new Set(), openImages: new Set() };
  applyViewChoices(fresh, round.view);
  assert.equal(fresh.folds.get("b3"), false); assert.ok(fresh.capOpen.has("b5:0") && fresh.openImages.has("b9") && fresh.rawTurns.get("user:t1") === true);
  const vp = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
  assert.match(vp, /if \(this\.pendingView\) \{ applyViewChoices\(this\.state, this\.pendingView\); this\.pendingView = null; \}/, "restored with the first batch, after the store's reset");
  assert.match(vp, /const view = viewChoices\(this\.state\);/, "…and saved with the position");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /rerender: \(\) => \{ viewport\.render\(\); viewport\.scheduleRemember\(\); \}/, "every choice schedules a save");
  assert.match(app, /addEventListener\("pagehide", \(\) => viewport\.remember\(\)\);/, "…and leaving saves at once");
  console.log("#114 view state cases passed");
}

// #115: gutters never enter a selection; each code pane has its bar; copy skips gutters and marks.
{
  const vm = readFileSync(new URL("../../claude-monitor/src/codex-ui/view-model.js", import.meta.url), "utf8");
  assert.match(vm, /<div class="codefoot">\$\{cut\.button\}\$\{bar\}<\/div><\/div>`/, "the bar and the expander share the pane's foot");
  assert.match(vm, /data-code-size="-1"[^>]*>A−<\/button><span class="code-size-val" data-code-size-val><\/span><button[^>]*data-code-size="1"/, "A− size A+");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /\[\.\.\.box\.querySelectorAll\("\.codecell"\)\]\.map\(cell => cell\.textContent\)\.join\("\\n"\)/, "copy joins the code cells only");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.codebox \.ln,\.codebox \.mark\{user-select:none\}/, "gutters and marks are unselectable");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /readingStep: delta => setReading\(\{ size: uiState\.reading\.size \+ delta \* SIZE_STEP \}\)/, "the bar drives the reading size");
  console.log("#115 code pane cases passed");
}

// #116: a tool row has a spot link and a hash lands on any record, its chain opened.
{
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /class="spot-link renderer-spot" type="button" data-spot-link="\$\{escapeText\(view\.id\)\}"/, "a tool row's spot link");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /const index = recordState\.records\.findIndex\(record => recordChain\(record, id, chain\)\);/, "a hash resolves through nested records");
  assert.match(app, /for \(const rid of chain\) recordState\.folds\.set\(rid, false\);/, "…opening the chain to it");
  assert.match(app, /if \(!viewport\.jumpToRecord\(index, "hash"\)\) return false;/, "…and lands there");
  console.log("#116 deep link cases passed");
}

// #100: stepping re-enters from the viewport once the current hit is off screen; the term lands in view.
{
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /const onScreen = !!box && box\.bottom >= view\.top && box\.top <= view\.bottom;/, "the current hit's mark decides whether the sequence continues");
  assert.match(app, /const k = matches\.findIndex\(index => index >= top\);/, "…else the first hit at or below the viewport top");
  assert.match(app, /recordState\.match = delta > 0 \? \(k >= 0 \? k : 0\) : \(k > 0 \? k - 1 : matches\.length - 1\);/, "forward from there, backward from the one above, wrapping");
  assert.match(app, /function landOnCurrentMark\(\) \{/, "the landing brings the term into view");
  const vp = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
  assert.match(vp, /if \(reveal === "search" \|\| reveal === "hash"\) \{\n\s*const openAll = view =>/, "a search or deep-link reveal opens the nested chain and its caps");
  console.log("#100 hit stepping cases passed");
}

// #101: the scope grammar and per-part ownership are shared; the app shell counts per class.
{
  assert.deepEqual(parseScope("ub:x"), { set: { u: true, a: false, t: false, o: false, b: true, r: false, e: false, w: false }, len: 3 }, "an order-free letter run then a colon");
  assert.deepEqual(parseScope("BU:x").set, parseScope("ub:x").set, "case-insensitive, order-free");
  assert.deepEqual(parseScope(":u:x"), { set: null, len: 1 }, "a leading colon escapes");
  assert.equal(parseScope("uu:x"), null, "a repeated letter is a word");
  assert.equal(parseScope("needle"), null);
  assert.deepEqual(scopeLetters(parseScope("wbu:x").set), ["u", "b"]); assert.deepEqual(activeLetters(parseScope("wbu:x").set), ["u", "b", "w"]);
  assert.equal(scopeMask(parseScope("ub:x").set), CLASS_BIT.u | CLASS_BIT.b);
  const record = { kind: "act", id: "b1", head: {}, body: [{ p: "md", h: "<p>Thinking</p>" }, { p: "blocks", items: [{ kind: "bash", id: "b2", head: { name: "Bash" }, body: [{ p: "pre", x: "NEEDLE out" }] }] }] };
  const tp = recordTextParts(record, stripTags, s => s.toLowerCase());
  assert.equal(tp.text.includes("needle out"), true, "lowercased per own text");
  assert.equal(tp.parts.length, 2); assert.equal(tp.parts[0].mask, CLASS_BIT.t); assert.equal(tp.parts[1].mask, CLASS_BIT.o | CLASS_BIT.b, "a nested tool owns its text");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  // The gate itself now lives in the shared module, and BOTH pages run it (#118).
  const searchModule = readFileSync(new URL("../../claude-replay-html/src/html/shared/search.js", import.meta.url), "utf8");
  assert.match(searchModule, /if \(!wanted \|\| part\.mask & wanted\) inScope \+= n;/, "stepping is gated by the scope");
  assert.match(app, /const inScope = countRecord\(text, parts, query, whole, wanted, classCounts\);/, "the app shell counts through the module");
  assert.match(app, /data-scope-count="\$\{key\}"/, "the scope rows carry counts");
  assert.match(app, /function applyScopeFromMenu\(\) \{/, "the buttons rewrite the box's prefix");
  const search = readFileSync(new URL("../../claude-replay-html/src/html/shared/search.js", import.meta.url), "utf8");
  assert.match(search, /^export \{ CLASS_BIT, CLASS_ORDER, MIN_NEEDLE, directMask, ownTextParts, recordText, recordTextParts, recordTextSize, LIVE_SEARCH_LIMIT, parseScope, scopeLetters, activeLetters, scopeMask, splitQuery, zeroCounts, countRecord, countLabel, writePrefix, stripTags, WORD_LEFT, WORD_RIGHT, wholeAt, countOcc \};\s*$/m);
  console.log("#101 scope cases passed");
}

// #104: above the shared haystack limit both pages search on Enter.
{
  assert.equal(LIVE_SEARCH_LIMIT, 10 * 1024 * 1024, "the owner's threshold");
  const record = { kind: "act", head: { name: "Bash" }, body: [{ p: "md", h: "<p>hi</p>" }, { p: "blocks", items: [{ kind: "bash", head: {}, body: [{ p: "pre", x: "x".repeat(100) }] }] }] };
  assert.equal(recordTextSize(record), 4 + 9 + 100, "head strings, part strings, nested records — without building the text");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /function searchIsLive\(\) \{ let n = 0; for \(const s of recordState\.recSizes\) n \+= s; return n <= LIVE_SEARCH_LIMIT; \}/, "live while small");
  assert.match(app, /if \(recordState\.pendingSearch\) \{ recordState\.pendingSearch = false; updateSearch\(true\); return; \}\n\s*stepSearch\(event\.shiftKey \? -1 : 1\);/, "Enter runs a pending search, else steps");
  console.log("#104 large-session search cases passed");
}

// #103: the process header's turn ordinal comes from the unit, not from a CSS counter.
{
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /data-turn="\$\{escapeText\(unit\.turn\)\}" data-turn-label="\$\{escapeText\(String\(unit\.turn\)\.padStart\(2, "0"\)\)\}"/, "the surface carries its turn");
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(comp, /<span class="process-surface-label" data-turn-label="\$\{escapeText\(String\(unit\.turn\)\.padStart\(2, "0"\)\)\}"/, "the label element carries it (attr() reads the pseudo-element's own element)");
  assert.match(css, /\.process-surface-label\[data-turn-label\]:after\{content:"Turn " attr\(data-turn-label\)\}/, "…and the label reads it, not the counter");
  console.log("#103 turn ordinal cases passed");
}

// #99: a turn's chrome never enters a selection.
{
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.turn \.spot-link,\.turn \.raw-toggle,\.turn \.turn-time,\.turn \.prompt-expand,[^{]*\{user-select:none\}/, "spot links, raw toggles, the time and the expander are unselectable");
  console.log("#99 selection cases passed");
}

// #117: a tool head's state, exit and duration are the shared module's reading of its chips.
{
  assert.equal(displayName("Edit"), "Update");
  assert.equal(displayName("MultiEdit"), "Update");
  assert.equal(displayName("Bash"), "Bash");
  const failed = toolHead({ name: "Bash", target: "cargo test", chips: [{ c: "fail", x: "exit 1 · 2.50s" }] });
  assert.deepEqual([failed.state, failed.exit, failed.duration, failed.failed], ["failed", 1, "2.50s", true]);
  assert.equal(stateLabel(failed), "failed · exit 1", "a failure names its word and its exit");
  const declined = toolHead({ chips: [{ c: "fail", x: "declined · 42ms" }] });
  assert.deepEqual([declined.state, declined.status, declined.duration, stateLabel(declined)], ["failed", "declined", "42ms", "declined"]);
  const long = toolHead({ chips: [{ x: "exit 0 · 1m 5s" }] });
  assert.deepEqual([long.state, long.exit, long.duration, stateLabel(long)], ["completed", 0, "1m 5s", "exit 0 · 1m 5s"]);
  const read = toolHead({ name: "Read", chips: [{ x: "12 lines" }] });
  assert.deepEqual([read.state, read.lines, stateLabel(read)], ["completed", 12, "12 lines"]);
  // "launched" is the LAUNCH EVENT of an async spawn, not liveness: present.rs spawn_chip writes
  // it whatever the spawn's status, and the terminal verb arrives on a separate AgentDone record.
  const launched = toolHead({ chips: [{ x: "3 tools · launched" }] });
  assert.deepEqual([launched.state, launched.status, stateLabel(launched)], ["completed", "launched", "3 tools · launched"], "a launch event is not a running head");
  assert.equal(toolHead({ chips: [{ x: "launched" }] }).running, undefined, "a head carries no liveness at all");
  const killed = toolHead({ chips: [{ c: "fail", x: "killed" }] });
  assert.deepEqual([killed.state, stateLabel(killed)], ["failed", "killed"], "a failure keeps the server's own word");
  assert.equal(toolHead({ chips: [{ x: "done" }] }).state, "completed");
  assert.equal(displayName("constructor"), "constructor", "the name map has no prototype");
  assert.equal(toolHead({ name: "Thinking" }).state, null, "a chipless head keeps its renderer's own word");
  assert.equal(stateLabel(toolHead({ chips: [{ c: "fail", x: "failed" }] })), "failed", "Claude's format has no exit code");
  // The view model consumes it — no regex over chip text remains — and the pill shows its label.
  const view = viewRecord({ kind: "bash", id: "b1", head: { name: "Bash", target: "cargo test", chips: [{ c: "fail", x: "exit 1 · 2.50s" }] }, body: [{ p: "pre", x: "error" }] });
  assert.deepEqual([view.state, view.error, view.exit, view.duration, view.pill], ["failed", true, 1, "2.50s", "failed · exit 1"]);
  const edit = viewRecord({ kind: "edit", id: "e1", head: { name: "Edit", target: "README.md", chips: [{ c: "add", x: "+1" }, { c: "del", x: "−1" }] }, body: [] });
  assert.deepEqual([edit.name, edit.state, edit.pill], ["Update", "completed", "+1 · −1"]);
  assert.equal(viewRecord({ kind: "think", id: "t1", head: {}, body: [{ p: "md", x: "hm" }] }).state, null);
  const spawn = viewRecord({ kind: "agent", id: "a1", head: { name: "Agent", chips: [{ x: "launched" }] }, body: [] });
  assert.deepEqual([spawn.state, spawn.running, spawn.pill], ["completed", false, "launched"], "a finished session's spawns are not left running");
  const cmd = viewRecord({ kind: "command", id: "c1", head: { badge: "/compact", preview: "", chips: [{ x: "12 lines" }] }, body: [] });
  assert.deepEqual([cmd.command.name, cmd.command.lines], ["compact", "12 lines"], "a command card's lines chip is the module's count");
  assert.equal(viewRecord({ kind: "command", id: "c2", head: { badge: "/compact", chips: [] }, body: [] }).command.lines, "", "…and absent when the wire carries none");
  const vm = readFileSync(new URL("../../claude-monitor/src/codex-ui/view-model.js", import.meta.url), "utf8");
  assert.doesNotMatch(vm, /fail\|error|running\|active|lines\$\/|\.find\(x => \//, "the state is not a regex over chip text");
  assert.match(vm, /from "\.\/shared\/tool-head\.js"/);
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /\$\{escapeText\(view\.state \? view\.pill : status === "completed" \? "" : status\)\}/, "the pill shows the module's label");
  console.log("#117 tool head cases passed");
}

// #122: a bare tool result reads as the classic page draws it — a Result row whose body wears
// the ⎿ gutter, from the shared module.
{
  assert.equal(RESULT_MARK, "⎿");
  assert.equal(
    resultBodyHtml("<pre>out</pre>", { result: "r", lead: "l", box: "b" }),
    '<div class="r"><span class="l">⎿</span><div class="b"><pre>out</pre></div></div>',
    "the mark and the box around the output"
  );
  // The server names a bare result and writes no `tool` field — only a call carries one.
  const bare = viewRecord({ kind: "tool", id: "r1", head: { name: "Result", target: "checked 42 files…" }, body: [{ p: "pre", x: "line one\nline two" }] });
  assert.deepEqual([bare.name, bare.summary, bare.bare], ["Result", "checked 42 files…", true]);
  const call = viewRecord({ kind: "bash", id: "b9", tool: "Bash", head: { name: "Bash", target: "echo hi" }, body: [{ p: "pre", x: "hi" }] });
  assert.equal(call.bare, false, "a tool call keeps this shell's own rail");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /return view\.bare \? resultBodyHtml\(html, APP_RESULT\) : html;/, "the app shell draws it through the module");
  assert.match(comp, /const APP_RESULT = \{ result: "renderer-result", lead: "renderer-result-lead", box: "renderer-result-box" \};/);
  const css = readFileSync(new URL("../../claude-monitor/src/codex-ui/production.css", import.meta.url), "utf8");
  assert.match(css, /\.renderer-result\{display:flex;gap:8px\}/, "…and styles it beside the output");
  console.log("#122 bare result cases passed");
}

// #121: an agent's own question to the reader is one card, drawn by both pages.
{
  assert.equal(isInteraction({ kind: "request_user_input" }), true);
  assert.equal(isInteraction({ kind: "something_else" }), false);
  assert.equal(isInteraction(null), false);
  const waiting = interactionCard({ kind: "request_user_input", resolved: false, answers: [] }, "Which shell should stay?");
  assert.deepEqual([waiting.state, waiting.icon, waiting.title, waiting.text], ["waiting", "?", "Waiting for user input", "Which shell should stay?"]);
  assert.match(waiting.meta, /Monitor cannot submit this native prompt/, "the question is the body; where to answer is the note");
  const bare = interactionCard({ kind: "request_user_input", resolved: false, answers: [] }, "");
  assert.equal(bare.meta, "", "…and the note is not said twice when there is no question");
  const done = interactionCard({ kind: "request_user_input", resolved: true, answers: [{ id: "shell", label: "Keep classic" }] }, "Which shell should stay?");
  assert.deepEqual([done.state, done.icon, done.title, done.answers.length], ["resolved", "✓", "User input received", 1]);
  const html = interactionHtml({ kind: "request_user_input", resolved: true, answers: [{ id: "shell", label: "Keep <classic>" }] }, "", { card: "c", icon: "i", copy: "p", meta: "m", answers: "as", answer: "a" });
  assert.match(html, /^<div class="c resolved"><span class="i" aria-hidden="true">✓<\/span>/);
  assert.match(html, /<span class="a"><span>Keep &lt;classic&gt;<\/span><small>shell<\/small><\/span>/, "an answer's label is escaped");
  const comp = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp, /return interactionHtml\(view\.interaction, view\.summary, APP_INTERACTION\);/, "the app shell draws the shared card");
  assert.doesNotMatch(comp, /Waiting for user input|Monitor cannot submit/, "…and holds none of the words itself");
  console.log("#121 request-for-input cases passed");
}

// #118: the search and the filter are one set of rules, and both pages run them.
{
  assert.deepEqual(CLASS_ORDER, ["u", "a", "t", "o", "b", "r", "e"]);
  assert.equal(MIN_NEEDLE, 2);
  const scoped = splitQuery("ub:needle");
  assert.deepEqual([scoped.needle, scoped.lc, scopeLetters(scoped.set), scoped.tooShort], ["needle", "needle", ["u", "b"], false]);
  assert.deepEqual([splitQuery("auto:").needle, splitQuery("auto:").set], ["auto:", null], "a pure run searches itself");
  assert.deepEqual([splitQuery(":ub:x").needle, splitQuery(":ub:x").set], ["ub:x", null], "a leading colon escapes");
  assert.equal(splitQuery("a").tooShort, true, "one character searches nothing");
  assert.equal(splitQuery("ab").tooShort, false);
  // The counts row is filled whatever the scope; only the RETURN is scoped. A record's own
  // text is the unscoped truth, so the bytes no part claims are counted too.
  const parts = [{ start: 0, end: 6, mask: CLASS_BIT.u }, { start: 6, end: 12, mask: CLASS_BIT.o | CLASS_BIT.b }];
  const counts = zeroCounts();
  assert.equal(countRecord("aa aa aa aa ", parts, "aa", false, CLASS_BIT.u, counts), 2, "scoped: the parts in scope");
  assert.deepEqual([counts.u, counts.b, counts.o, counts.t], [2, 2, 2, 0], "…and every class row still fills");
  const open = zeroCounts();
  assert.equal(countRecord("aa aa aa aa ", parts, "aa", false, 0, open), 4, "unscoped: the record's own text");
  assert.deepEqual([open.u, open.b], [2, 2]);
  assert.equal(countRecord("one two one", [{ start: 0, end: 11, mask: CLASS_BIT.u }], "one", true, CLASS_BIT.u, null), 2, "whole words");
  assert.equal(countLabel(1, null, false), "1 hit");
  assert.equal(countLabel(12, parseScope("ub:x").set, false), "12 hits in ub");
  assert.equal(countLabel(3, parseScope("w:x") ? parseScope("w:x").set : null, true), "3 hits · whole words", "whole words alone scopes nothing");
  assert.equal(writePrefix("ub:needle", ["a"]), "a:needle");
  assert.equal(writePrefix("ub:needle", []), "needle", "no letters, no prefix");
  assert.equal(writePrefix("  needle", ["u", "b"]), "ub:needle");
  // The filter chain: a parent whose child matches is on the chain, and the walk says which.
  const seen = [];
  const tree = { id: "p", kind: "act", body: [{ p: "blocks", items: [{ id: "c1", kind: "bash", body: [] }, { id: "c2", kind: "read", body: [] }] }] };
  assert.equal(chainWalk(tree, rec => rec.kind === "bash", (rec, direct) => seen.push([rec.id, direct])), true);
  assert.deepEqual(seen, [["c1", true], ["p", false]], "the match, then the ancestor that contains it");
  assert.equal(chainWalk(tree, rec => rec.kind === "write", null), false, "nothing under it, no chain");
  const app = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app, /const q = splitQuery\(raw\);/, "the app shell splits the query through the module");
  assert.match(app, /byId\("transcriptSearchCount"\).textContent = query \? countLabel\(total, set, whole\) : "";/);
  assert.match(app, /input\.value = writePrefix\(input\.value, set \? activeLetters\(set\) : \[\]\);/);
  assert.match(app, /return chainWalk\(/, "…and walks the filter chain through the module");
  assert.doesNotMatch(app, /function elementMask|function wholeAtText/, "its own copies are gone");
  // The mark gate is the record's KIND through the shared class table — the classic page's rule.
  // The old gate read the row's DISPLAY name, so an Edit (shown as "Update") was counted under
  // `e:` and marked nowhere.
  assert.match(app, /return !wanted \|\| !!\(directMask\(row\?\.dataset\.recordKind \|\| ""\) & wanted\);/);
  assert.match(app, /kindInScope\(walker\.currentNode\.parentElement\.closest\("\[data-record-kind\]"\), wanted\)/);
  const comp118 = readFileSync(new URL("../../claude-monitor/src/codex-ui/components.js", import.meta.url), "utf8");
  assert.match(comp118, /data-record-kind="\$\{escapeText\(view\.raw\?\.kind \|\| view\.renderer\)\}"/, "every renderer row carries its record kind");
  assert.match(comp118, /class="turn user" data-kind="user" data-record-kind="user"/);
  assert.match(comp118, /class="turn user command" data-kind="user" data-record-kind="command"/);
  assert.match(app, /const key = unitAtTop\(\);\n  return recordState\.units\.find\(unit => unit\.key === key\)\?\.from \?\? 0;/, "a unit key is resolved to a record index");
  assert.doesNotMatch(app, /unitAtTop\(\)\?\.from/, "…never read as one");
  const js = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(js, /var q = shared\.splitQuery\(v\);/, "the classic page runs the same split");
  assert.match(js, /qc\.textContent = hr[\s\S]{0,120}shared\.countLabel\(totalHits, searchScope, whole\);/);
  assert.match(js, /q\.value = shared\.writePrefix\(q\.value, letters\);/);
  assert.match(js, /return shared\.chainWalk\(b, function \(rec\) \{ return filterMatchesDirect\(rec, want\); \}, null\);/);
  console.log("#118 shared search and filter cases passed");
}

// #107: the virtual window's arithmetic is one module, and it is the app shell's own numbers.
// These run DIFFERENTIALLY against transcriptions of the bodies viewport.js had before the
// extraction — the two implementations exist side by side only at this moment.
{
  const h = [40, 132, 8, 300, 132];
  const heightAt = i => h[i];
  const oldPrefix = (count, at) => { const p = [0]; for (let i = 0; i < count; i++) p.push(p.at(-1) + at(i)); return p; };
  const sums = prefixSums(5, heightAt);
  assert.deepEqual(sums, oldPrefix(5, heightAt));
  assert.deepEqual(sums, [0, 40, 172, 180, 480, 612]);
  const oldIndexAt = (p, count, y) => { if (!count) return 0; let lo = 0, hi = count; while (lo < hi) { const mid = (lo + hi) >> 1; if (p[mid + 1] > y) hi = mid; else lo = mid + 1; } return Math.min(lo, count - 1); };
  for (const y of [-50, 0, 39, 40, 171, 172, 179, 180, 479, 480, 611, 612, 9000]) {
    assert.equal(indexAt(sums, 5, y, true), oldIndexAt(sums, 5, y), `clamped index at ${y}`);
  }
  assert.equal(indexAt(sums, 5, 9000, false), 5, "unclamped, an offset past the end is past the end — the classic page's reading");
  assert.equal(indexAt([0], 0, 10, true), 0, "no items, no index");
  const oldRangeForScroll = (p, count, top, height, over) => ({ lo: oldIndexAt(p, count, Math.max(0, top - over)), hi: Math.min(count, oldIndexAt(p, count, top + height + over) + 1) });
  for (const top of [0, 100, 400, 612]) {
    assert.deepEqual(rangeForScroll(sums, 5, top, 200, 150), oldRangeForScroll(sums, 5, top, 200, 150), `range at ${top}`);
  }
  const oldRangeAround = (index, count, at, height, over) => { let lo = index, hi = index + 1, above = over, below = height + over; while (lo > 0 && above > 0) { lo--; above -= at(lo); } while (hi < count && below > 0) { below -= at(hi); hi++; } return { lo, hi }; };
  for (const index of [0, 2, 4]) {
    assert.deepEqual(rangeAround(index, 5, heightAt, 200, 150), oldRangeAround(index, 5, heightAt, 200, 150), `around ${index}`);
  }
  assert.deepEqual(clampRange(-5, 99, 5), { lo: 0, hi: 5 });
  assert.deepEqual(clampRange(4, 2, 5), { lo: 4, hi: 4 }, "a backwards range is empty, not inverted");
  assert.deepEqual(padHeights(sums, 1, 3, 5), { top: 40, bottom: 432 });
  assert.deepEqual(padHeights(sums, 0, 5, 5), { top: 0, bottom: 0 }, "everything mounted, no pads");
  // The measure threshold: more than a pixel tall, more than half a pixel different.
  assert.equal(heightChanged(132, 140, 1, 0.5), true);
  assert.equal(heightChanged(132, 132.4, 1, 0.5), false, "sub-pixel noise is not a height");
  assert.equal(heightChanged(132, 0.5, 1, 0.5), false, "an element that is not laid out yet is not a height");
  assert.equal(heightChanged(30, 40, 0, 0.5), true, "the classic page's floor is zero");
  assert.equal(correction(210, 200, 1), 10, "put the anchored row back where it was");
  assert.equal(correction(200.5, 200, 1), 0, "…but not by a pixel of noise");
  const items = [
    { index: 0, top: -80, bottom: -10, height: 70 },
    { index: 1, top: -10, bottom: 120, height: 130 },
    { index: 2, top: 120, bottom: 400, height: 280 },
  ];
  assert.equal(firstVisible(items, 0, 500, 1, false).index, 1, "the first row the reader can see");
  assert.equal(firstVisible(items, 0, 500, 1, true).index, 1);
  assert.equal(firstVisible([{ index: 9, top: 600, bottom: 700, height: 100 }], 0, 500, 1, false), null, "past the last of them there is no anchor at all");
  assert.equal(firstVisible([{ index: 3, top: -5, bottom: -5, height: 0 }, items[2]], 0, 500, 1, true).index, 2, "a row laid out to nothing is no anchor");
  // Rule 7, with each page's slacks.
  assert.equal(classifyScroll(false, true, 1, 2, 2, 80), "follow");
  assert.equal(classifyScroll(false, true, 40, 2, 2, 80), "none");
  assert.equal(classifyScroll(true, true, 40, 2, 2, 80), "unfollow", "the app shell decides on the true end in both directions (#127)");
  assert.equal(classifyScroll(true, true, 40, 2, 80, 80), "none", "the classic page holds through a nudge");
  assert.equal(classifyScroll(true, false, 120, 2, 2, 80), "heal", "displacement while pinned is healed");
  assert.equal(classifyScroll(false, false, 900, 2, 2, 80), "none", "…and means nothing when unpinned");
  const vp = readFileSync(new URL("../../claude-monitor/src/codex-ui/viewport.js", import.meta.url), "utf8");
  assert.match(vp, /from "\.\/shared\/virtual-window\.js";/, "the app shell drives the module");
  // Step 4: the engine itself is shared, and this shell is its consumer — the sums, the window,
  // the anchor, the observers, the follow state, the thumb and the converge run from one place.
  const engine = readFileSync(new URL("../../claude-replay-html/src/html/shared/virtual-window.js", import.meta.url), "utf8");
  assert.match(engine, /this\.prefix = prefixSums\(this\.count, index => this\.heightOf\(index\)\);/);
  assert.match(engine, /const verdict = classifyScroll\(this\.following, user, this\.gapToBottom\(\), this\.slacks\.acquire, this\.slacks\.hold, this\.slacks\.heal\);/);
  assert.match(vp, /slacks: \{ acquire: ACQUIRE_SLACK, hold: ACQUIRE_SLACK, heal: HOLD_SLACK \},/, "this shell decides on the true end in both directions (#127)");
  assert.doesNotMatch(vp, /new ResizeObserver|addEventListener\("scroll"/, "the observers and the scroll listener are the engine's now");
  const module = readFileSync(new URL("../../claude-replay-html/src/html/shared/virtual-window.js", import.meta.url), "utf8");
  // The RULES half of the module is numbers in, numbers out: no layout read can hide among
  // them, which is what lets these tests run in node at all. (`scrollTop` names a parameter.)
  // The engine below the marker drives the DOM by definition; that this import worked at all is
  // the guard that it touches nothing at module TOP level, where node has no document.
  const rules = module.slice(0, module.indexOf("/* ── the engine"));
  assert.ok(rules.length > 2000 && module.includes("/* ── the engine"), "the file keeps its two halves");
  assert.doesNotMatch(rules, /document\.|ResizeObserver|\.getBoundingClientRect\(|\.scrollTop|\.style\.|performance\.now\(|setTimeout\(|addEventListener/, "the rules are numbers in, numbers out");
  // Step 3: the estimate is a FLOOR per unit type — under the real height, never over, so
  // learning a height only grows the page below the reader (rule 5).
  assert.match(vp, /const ESTIMATES = \{ user: 44, assistant: 40, process: 34 \};/);
  assert.match(vp, /estimateAt\(index\) \{ return ESTIMATES\[this\.units\[index\]\?\.type\] \|\| ESTIMATE; \}/, "the estimate is this shell's answer to the engine's question");
  // Step 2: the classic page — the reference — runs the same arithmetic, with its own numbers.
  const cls = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(cls, /if \(!prefix\) prefix = shared\.prefixSums\(records\.length, effH\);/, "the classic sums stay LAZY and shared");
  assert.match(cls, /return shared\.indexAt\(P\(\), records\.length, y, false\);/, "…and its search stays unclamped");
  assert.match(cls, /var pads = shared\.padHeights\(P\(\), loIdx, hiIdx, records\.length\);/);
  assert.match(cls, /var first = shared\.firstVisible\(items, 0, Infinity, 0, false\);/, "no epsilon above the fold on this page");
  assert.match(cls, /window\.scrollBy\(0, shared\.correction\(e\.getBoundingClientRect\(\)\.top, a\.top, 1\)\);/);
  assert.match(cls, /var verdict = shared\.classifyScroll\(following, performance\.now\(\) - lastUserInput < USER_MS, gapToBottom\(\), PIN_SLACK, BOTTOM_SLACK, BOTTOM_SLACK\);/, "the classic page holds the pin through a nudge");
  console.log("#107 virtual window cases passed");
}

// #125: a task reads as the queue's own board shows it — a glyph, chips, the stamps on one
// line, and labelled sections — and both pages render the same anatomy.
{
  const done = {
    id: "125", subject: "Render tasks the way the board does", status: "Completed",
    owner: "claude-code/hong@aries-black", created: "2026-09-04T18:23:00Z",
    claimed: "2026-09-04T22:14:00Z", completed: "2026-09-04T22:53:00Z",
    description: "the prose", accept: ["one", "two"], outcome: "shipped", checks: 1,
    blockedBy: ["7"], log: [{ ts: "2026-09-04T22:49:00Z", by: "claude", msg: "found the seam" }],
  };
  assert.deepEqual([cardStatus("InProgress"), cardStatus("Completed"), cardStatus("pending"), cardStatus("")], ["in_progress", "completed", "pending", "pending"]);
  assert.deepEqual([taskGlyph("Pending"), taskGlyph("InProgress"), taskGlyph("Completed"), taskGlyph("cancelled"), taskGlyph("Completed", true)], ["○", "◐", "✓", "✗", "◌"]);
  assert.equal(taskStamp("2026-09-04T18:23:00Z"), "09-04 18:23", "the month-day and the clock, nothing else");
  assert.equal(taskStamp(""), "");
  assert.equal(taskDates(done), "created 09-04 18:23 · claimed 09-04 22:14 · completed 09-04 22:53");
  assert.equal(taskDates({ created: "2026-09-04T18:23:00Z", updated: "2026-09-05T09:00:00Z", status: "InProgress" }), "created 09-04 18:23 · updated 09-05 09:00", "still open: when it last moved");
  assert.deepEqual(taskChips(done).map(c => c.kind), ["status", "owner", "checks", "blocked"]);
  assert.equal(taskChips(done)[0].text, "completed");
  assert.equal(taskChips({ status: "Pending", deferred: true })[0].text, "deferred");
  assert.equal(taskRowMeta(done), "claude-code/hong@aries-black · blocked by #7 · 1 check");
  assert.equal(taskRowMeta({ id: "1", subject: "bare" }), "", "nothing to say leaves the row one line");
  assert.deepEqual(taskSections(done).map(s => s.label), ["description", "acceptance", "outcome", "worklog"]);
  assert.deepEqual(taskSections(done).at(-1).log, [{ ts: "09-04 22:49", by: "claude", msg: "found the seam" }]);
  const classes = { card: "c", head: "h", glyph: "g", id: "i", title: "t", chips: "cs", chip: "ch", dates: "d", section: "s", label: "l", body: "b", item: "it", outcome: "o", log: "lg", logTime: "lt", logMsg: "lm", logBy: "lb" };
  const html = taskCardHtml(done, classes);
  assert.match(html, /<span class="g" data-state="completed">✓<\/span><span class="i">#125<\/span>/);
  assert.match(html, /<div class="d">created 09-04 18:23 · claimed 09-04 22:14 · completed 09-04 22:53<\/div>/);
  assert.match(html, /<div class="s o"><span class="l">outcome<\/span>/, "the outcome is a callout");
  assert.match(html, /<span class="lt">09-04 22:49<\/span><div class="lm">found the seam<span class="lb"> — claude<\/span>/);
  assert.match(taskCardHtml({ id: "1", subject: "<b>x</b>" }, classes), /&lt;b&gt;x&lt;\/b&gt;/, "a subject is escaped");
  const app125 = readFileSync(new URL("../../claude-monitor/src/codex-ui/app.js", import.meta.url), "utf8");
  assert.match(app125, /\$\{taskCardHtml\(\{ \.\.\.task, id: key, blockedBy: d\.blockedBy, blocks: d\.blocks \}, APP_TASK\)\}/, "the app shell's popover shows the card");
  const js125 = readFileSync(new URL("../../claude-replay-html/src/html/export.js", import.meta.url), "utf8");
  assert.match(js125, /det\.insertAdjacentHTML\("beforeend", shared\.taskCardHtml\(card\(t\), CLASSIC_TASK\)\);/, "…and so does the classic page's panel");
  assert.match(js125, /row\.appendChild\(el\("span", "task-glyph", shared\.taskGlyph\(t\.status, t\.deferred\)\)\);/);
  assert.match(js125, /var meta = shared\.taskRowMeta\(card\(t\)\);/);
  console.log("#125 task card cases passed");
}
