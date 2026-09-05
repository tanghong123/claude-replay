// How a TASK reads, for both pages (#125). The owner's words: "the rendering of tasks is not
// easy to read" — a wall of one-weight prose with no dates, no owner, no outcome and no
// worklog, where the queue's own board (agentdev's taskq-board) shows a glyph, chips, a
// created·claimed·completed line and labelled sections. This module holds that anatomy: what a
// status looks like, what the chips say, how the stamps read, and the card's markup — each page
// passing its own class names, as with every other shared module.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

const escapeTask = value => String(value ?? "").replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);

/** The board's glyphs: pending, running, done, cancelled — and a parked task keeps its own. */
const TASK_GLYPH = { pending: "○", in_progress: "◐", completed: "✓", cancelled: "✗" };

/** The wire's status vocabulary is the engine's (`Pending`/`InProgress`/`Completed`); the board's
 *  is lower-case with an underscore. One reading for both. */
function taskStatus(status) {
  const s = String(status || "").toLowerCase().replace(/\s+/g, "_");
  if (s === "inprogress" || s === "in_progress" || s === "running") return "in_progress";
  if (s === "completed" || s === "done") return "completed";
  if (s === "cancelled" || s === "canceled") return "cancelled";
  if (s === "pending" || s === "") return "pending";
  return s;
}

function taskGlyph(status, deferred) {
  return deferred ? "◌" : TASK_GLYPH[taskStatus(status)] || "·";
}

/** "09-04 18:23" — the board's stamp: the month-day and the clock, nothing else. A reader
 *  scanning a queue needs the ORDER of the day, not the year they already know. */
function taskStamp(iso) {
  const s = String(iso || "");
  return s.length >= 16 ? s.slice(5, 10) + " " + s.slice(11, 16) : "";
}

/** "created 09-04 18:23 · claimed 09-04 22:14 · completed 09-04 22:53" — a task's life on one
 *  line, in the order it happened; a task still open says when it last moved instead. */
function taskDates(task) {
  const parts = [];
  const created = taskStamp(task.created);
  const claimed = taskStamp(task.claimed);
  const completed = taskStamp(task.completed);
  const updated = taskStamp(task.updated);
  if (created) parts.push("created " + created);
  if (claimed) parts.push("claimed " + claimed);
  if (completed) parts.push((taskStatus(task.status) === "cancelled" ? "closed " : "completed ") + completed);
  else if (updated) parts.push("updated " + updated);
  return parts.join(" · ");
}

/** The chips beside the title: what state it is in, who holds it, what gates it, what blocks
 *  it. Each is `{kind, text}` — the page decides how a chip looks. */
function taskChips(task) {
  const status = taskStatus(task.status);
  const chips = [{ kind: "status", state: status, glyph: taskGlyph(status, task.deferred), text: task.deferred ? "deferred" : status.replace("_", " ") }];
  if (task.owner) chips.push({ kind: "owner", text: String(task.owner) });
  if (task.checks) chips.push({ kind: "checks", text: task.checks === 1 ? "1 check gates done" : `${task.checks} checks gate done` });
  const ids = list => list.map(id => "#" + id).join(" ");
  if (task.blockedBy && task.blockedBy.length) chips.push({ kind: "blocked", text: "blocked by " + ids(task.blockedBy) });
  if (task.blocks && task.blocks.length) chips.push({ kind: "blocks", text: "blocks " + ids(task.blocks) });
  return chips;
}

/** A pane ROW's second line: the little that tells a reader whether this row is theirs to act
 *  on. Empty when there is nothing to say, and then the row stays one line. */
function taskRowMeta(task) {
  const bits = [];
  if (task.deferred) bits.push(task.deferred_reason ? "parked · " + task.deferred_reason : "parked");
  if (task.owner) bits.push(task.owner);
  if (task.blockedBy && task.blockedBy.length) bits.push("blocked by " + task.blockedBy.map(id => "#" + id).join(" "));
  if (task.checks) bits.push(task.checks === 1 ? "1 check" : task.checks + " checks");
  return bits.join(" · ");
}

/** The card's sections, in the order a reader wants them: why it is parked, what it asks for,
 *  what it must satisfy, what came of it, and what was written along the way. Each is
 *  `{label, kind, text | items | log}` so a page renders prose, a list and a worklog its own
 *  way — the classic page as plain nodes, the app shell as markdown. */
function taskSections(task) {
  const out = [];
  if (task.deferred && task.deferred_reason) out.push({ label: "parked because", kind: "text", text: task.deferred_reason });
  if (task.description) out.push({ label: "description", kind: "text", text: task.description });
  if (task.accept && task.accept.length) out.push({ label: "acceptance", kind: "items", items: task.accept.map(String) });
  if (task.outcome) out.push({ label: "outcome", kind: "outcome", text: task.outcome });
  if (task.log && task.log.length) {
    out.push({ label: "worklog", kind: "log", log: task.log.map(e => ({ ts: taskStamp(e.ts), by: e.by || "", msg: e.msg || "" })) });
  }
  return out;
}

/** The whole detail card as markup, with the page's class names. `classes` names every part:
 *  card, head, glyph, id, title, chips, chip, dates, section, label, body, item, outcome,
 *  log, logTime, logMsg, logBy. */
function taskCardHtml(task, classes) {
  const status = taskStatus(task.status);
  const chips = taskChips(task)
    .map(c => `<span class="${classes.chip} ${classes.chip}-${c.kind}">${c.glyph ? `<span class="${classes.glyph}" data-state="${escapeTask(c.state)}">${c.glyph}</span>` : ""}${escapeTask(c.text)}</span>`)
    .join("");
  const dates = taskDates(task);
  const sections = taskSections(task).map(section => {
    let body = "";
    if (section.kind === "items") {
      body = `<div class="${classes.body}">${section.items.map(item => `<div class="${classes.item}">· ${escapeTask(item)}</div>`).join("")}</div>`;
    } else if (section.kind === "log") {
      body = `<div class="${classes.log}">${section.log.map(e => `<span class="${classes.logTime}">${escapeTask(e.ts)}</span><div class="${classes.logMsg}">${escapeTask(e.msg)}${e.by ? `<span class="${classes.logBy}"> — ${escapeTask(e.by)}</span>` : ""}</div>`).join("")}</div>`;
    } else {
      body = `<div class="${classes.body}">${escapeTask(section.text)}</div>`;
    }
    return `<div class="${classes.section}${section.kind === "outcome" ? " " + classes.outcome : ""}"><span class="${classes.label}">${escapeTask(section.label)}</span>${body}</div>`;
  }).join("");
  return `<div class="${classes.card}" data-task-status="${escapeTask(status)}">`
    + `<div class="${classes.head}"><span class="${classes.glyph}" data-state="${escapeTask(status)}">${taskGlyph(status, task.deferred)}</span>`
    + `<span class="${classes.id}">#${escapeTask(task.id)}</span>`
    + `<span class="${classes.title}">${escapeTask(task.subject || "")}</span></div>`
    + `<div class="${classes.chips}">${chips}</div>`
    + (dates ? `<div class="${classes.dates}">${escapeTask(dates)}</div>` : "")
    + sections
    + `</div>`;
}

export { TASK_GLYPH, taskStatus, taskGlyph, taskStamp, taskDates, taskChips, taskRowMeta, taskSections, taskCardHtml };
