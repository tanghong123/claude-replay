export const escapeText = value => String(value ?? "").replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);
export const plainText = record => JSON.stringify(record).replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();

/** The user-turn index the reader is AT: the last user turn at or before the unit `atKey`
 *  (the unit at the viewport top), or -1 when none is. DOM-free — the outline's focus (#52)
 *  and the `]`/`[` stepping share it, so the pane and the keys agree on "the current turn". */
export function currentTurnIndex(units = [], atKey = null) {
  const at = atKey == null ? -1 : units.findIndex(unit => unit.key === atKey);
  let current = -1;
  let turn = -1;
  for (let i = 0; i < units.length; i++) {
    if (units[i].type !== "user") continue;
    turn++;
    if (i <= at) current = turn;
    else break;
  }
  return current;
}

export function taskStatus(value) {
  const status = String(value || "pending").replace(/[\s-]+/g, "_").toLowerCase();
  if (status === "completed" || status === "done") return "completed";
  if (status === "inprogress" || status === "in_progress" || status === "running") return "in_progress";
  return "pending";
}

// Task meta deliberately carries current state, not a transcript position. Resolve each row to
// the latest record that actually mentions its stable subject/id so Outline navigation remains
// useful without inventing a second backend contract. Plan snapshots commonly resolve several
// rows to the same TodoWrite/update_plan result; that is the honest source record for all of them.
export function taskRecordTargets(tasks = [], records = []) {
  const unresolved = tasks.map((task, index) => ({
    key: String(task.id ?? index),
    subject: String(task.subject || task.title || "").trim().toLowerCase(),
    id: String(task.id ?? "").trim().toLowerCase()
  }));
  const targets = new Map();
  for (let recordIndex = records.length - 1; recordIndex >= 0 && targets.size < unresolved.length; recordIndex--) {
    const record = records[recordIndex];
    const name = String(record?.head?.name || record?.tool || "").toLowerCase();
    if (record?.kind !== "task" && record?.kind !== "tool" && !/task|todo|plan|goal/.test(name)) continue;
    const text = plainText(record).toLowerCase();
    for (const task of unresolved) {
      if (targets.has(task.key)) continue;
      const subjectHit = task.subject && text.includes(task.subject);
      const idHit = task.id && new RegExp(`(?:^|[^a-z0-9])#${task.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?:$|[^a-z0-9])`, "i").test(text);
      if (subjectHit || idHit) targets.set(task.key, recordIndex);
    }
  }
  return targets;
}

export function agentRecordTargets(agents = [], records = []) {
  const wanted = new Set(agents.map(agent => String(agent.id || "")).filter(Boolean));
  const targets = new Map();
  const visit = (record, recordIndex) => {
    if (!record || typeof record !== "object") return;
    const head = record.head || {};
    let child = String(head.child_id || "");
    if (!child && head.child) {
      try { child = new URL(String(head.child), "http://monitor.local/").searchParams.get("session") || ""; }
      catch (_) { child = ""; }
    }
    if (wanted.has(child) && !targets.has(child)) targets.set(child, recordIndex);
    for (const part of record.body || []) if (part?.p === "blocks") for (const item of part.items || []) visit(item, recordIndex);
  };
  records.forEach(visit);
  return targets;
}

const toolKinds = new Set(["bash", "read", "write", "edit", "skill", "tool"]);
const processKinds = new Set(["think", "act", "bash", "read", "write", "edit", "skill", "tool", "agent", "task", "queue", "command", "compaction", "attachment", "system", "context", "record", "file"]);

// The tasks pane's order (#56): three groups — completed, running, pending — and anything the
// status vocabulary does not know last, each group by id: numeric ids as numbers (2 before 10),
// taskq's plain ids before the user tier's `u12`, non-numeric ids after both by their text, and
// ties by the order the record stream carried them. Pure, so the contract pins it.
const TASK_GROUPS = Object.freeze([
  { key: "completed", label: "Completed" },
  { key: "in_progress", label: "Running" },
  { key: "pending", label: "Pending" },
  { key: "other", label: "Other" },
]);

export function taskGroupKey(status) {
  const s = String(status ?? "pending").replace(/[\s-]+/g, "_").toLowerCase();
  if (s === "completed" || s === "done") return "completed";
  if (s === "inprogress" || s === "in_progress" || s === "running") return "in_progress";
  if (s === "pending" || s === "") return "pending";
  return "other";
}

function taskIdKey(id) {
  const m = /^(u?)(\d+)$/i.exec(String(id ?? "").trim());
  return m ? [0, m[1] ? 1 : 0, Number(m[2]), ""] : [1, 0, 0, String(id ?? "")];
}

function compareIds(a, b) {
  const ka = taskIdKey(a), kb = taskIdKey(b);
  for (let i = 0; i < 3; i++) if (ka[i] !== kb[i]) return ka[i] - kb[i];
  return ka[3] < kb[3] ? -1 : ka[3] > kb[3] ? 1 : 0;
}

/** The tasks as `{ task, index }` rows (the stream's index kept for the record targets), in
 *  the pane's order. */
export function taskOrder(tasks = []) {
  const rank = Object.fromEntries(TASK_GROUPS.map((g, i) => [g.key, i]));
  return tasks
    .map((task, index) => ({ task, index, group: taskGroupKey(task?.status) }))
    .sort((a, b) => rank[a.group] - rank[b.group] || compareIds(a.task?.id, b.task?.id) || a.index - b.index);
}

/** The non-empty groups, in order, each with its rows. */
export function taskGroups(tasks = []) {
  const rows = taskOrder(tasks);
  return TASK_GROUPS.map(g => ({ ...g, rows: rows.filter(r => r.group === g.key) })).filter(g => g.rows.length);
}

/** Where the tasks pane should center (#57), over the rows `taskOrder` produced: the middle of
 *  the running run when there is one; otherwise the boundary between the completed rows and
 *  the rest — the first row after the completed ones, by its top edge — so a reader sees what
 *  is done above and what waits below. All done → the last row's bottom; all pending → the
 *  first row's top; no rows → null. `edge` says which edge of the row goes to the center. */
export function taskCenterTarget(rows = []) {
  if (!rows.length) return null;
  const running = rows.map((r, i) => (r.group === "in_progress" ? i : -1)).filter(i => i >= 0);
  if (running.length) return { index: running[Math.floor((running.length - 1) / 2)], edge: "middle", why: "running" };
  const firstOpen = rows.findIndex(r => r.group !== "completed");
  if (firstOpen < 0) return { index: rows.length - 1, edge: "bottom", why: "all-done" };
  if (firstOpen === 0) return { index: 0, edge: "top", why: "all-pending" };
  return { index: firstOpen, edge: "top", why: "boundary" };
}

/** What a task's details popover shows (#60), from the meta record's task and the record index
 *  where its status was last recorded (null when the stream did not keep it). The description
 *  is split into paragraphs on blank lines — the meta record carries it as text, not markup. */
export function taskDetails(task = {}, target = null) {
  const status = taskGroupKey(task.status);
  const label = { completed: "Completed", in_progress: "Running", pending: "Pending", other: String(task.status || "unknown") }[status];
  const text = String(task.description || "").trim();
  const paragraphs = text ? text.split(/\n\s*\n/).map(p => p.replace(/\s*\n\s*/g, " ").trim()).filter(Boolean) : [];
  return {
    id: String(task.id ?? ""),
    subject: String(task.subject || task.title || "").trim() || `Task ${task.id ?? ""}`.trim(),
    status, label,
    activeForm: String(task.active_form || task.activeForm || "").trim(),
    paragraphs,
    blockedBy: Array.isArray(task.blocked_by) ? task.blocked_by.map(String) : Array.isArray(task.blockedBy) ? task.blockedBy.map(String) : [],
    blocks: Array.isArray(task.blocks) ? task.blocks.map(String) : [],
    target: target == null ? null : Number(target),
  };
}

/** The session's published artifacts (#78), as the classic page's `collectArtifacts` groups
 *  them: one entry per URL — the stable identity; a republish keeps it — in first-seen order,
 *  with how many times it was published, the latest name / icon / description, and the index
 *  of the record that published it last. Walks nested blocks the way the classic page does. */
export function artifactRoster(records = []) {
  const by = new Map();
  const scan = (block, at) => {
    const a = block?.head?.artifact;
    if (a && a.url) {
      const e = by.get(a.url) || { url: a.url, count: 0, name: "", icon: "", desc: "", at };
      e.count += 1;
      e.name = a.name || e.name;
      e.icon = a.icon || e.icon;
      e.desc = a.desc || e.desc;
      e.at = at;
      by.set(a.url, e);
    }
    for (const part of block?.body || []) if (part.p === "blocks") for (const item of part.items || []) scan(item, at);
  };
  records.forEach((record, i) => scan(record, i));
  return [...by.values()];
}

/** Tokens as the usage panel writes them (the html crate's `human_tokens`): 0, 999, 8.6K,
 *  594.7K, 1.20M. */
export function humanTokens(n) {
  const v = Number(n) || 0;
  if (v === 0) return "0";
  if (v >= 1e6) return `${(v / 1e6).toFixed(2)}M`;
  if (v >= 1e3) return `${(v / 1e3).toFixed(1)}K`;
  return String(v);
}

/** What the turns pane draws for a compaction record (#86): a glyph for how it happened —
 *  automatic (the context filled) or by hand (`/compact`) — its tooltip, and the context size
 *  from → to when the record knows both. No prose. */
export function compactionTick(head = {}) {
  const manual = head.compact_trigger === "manual";
  const pre = Number(head.compact_pre) || 0, post = Number(head.compact_post) || 0;
  return {
    trigger: manual ? "manual" : "auto",
    glyph: manual ? "✂" : "⟳",
    title: manual ? "Compacted by hand (/compact)" : "Compacted automatically — the context filled",
    sizes: pre > 0 && post > 0 ? `${humanTokens(pre)} → ${humanTokens(post)}` : "",
  };
}

export function viewRecord(record) {
  const head = record.head || {};
  if (record.kind === "user") return { t: "user", id: record.id, html: partsHtml(record.body), markdown: true, source: record };
  if (record.kind === "assistant") return { t: "assistant", id: record.id, phase: record.phase || "unknown", presentation: head.presentation || "", html: partsHtml(record.body), markdown: true, source: record };
  if (record.kind === "think") return rendererRecord(record, "thinking", "Thinking");
  if (record.kind === "act") return rendererRecord(record, "activity", "Activity");
  if (record.kind === "agent") return rendererRecord(record, "agent", head.name || head.badge || "Agent");
  if (record.kind === "queue") return rendererRecord(record, "queue", "Queued input");
  if (record.kind === "attachment") return rendererRecord(record, "attachment", head.att_name || "Attachment");
  if (record.kind === "compaction") return rendererRecord(record, "context", "Context compacted");
  if (record.kind === "command") return rendererRecord(record, "system", head.name || "Command");
  if (record.kind === "task") return rendererRecord(record, "task", head.name || "Task");
  if (toolKinds.has(record.kind)) return rendererRecord(record, record.kind, head.name || record.tool || record.kind);
  if (processKinds.has(record.kind)) return rendererRecord(record, record.kind, head.name || record.kind);
  return rendererRecord(record, "fallback", `Unknown · ${record.kind || "record"}`);
}

function rendererRecord(record, renderer, name) {
  const head = record.head || {};
  const children = [];
  for (const part of record.body || []) if (part.p === "blocks") for (const item of part.items || []) children.push(viewRecord(item));
  const chips = head.chips || [];
  const chipText = chips.map(c => c.x).filter(Boolean).join(" · ");
  return {
    t: renderer === "thinking" ? "thinking" : renderer === "activity" ? "activity" : renderer === "task" ? "task" : renderer === "agent" ? "agent" : "tool",
    renderer, id: record.id, name, summary: head.target || head.preview || head.summary || "",
    state: chipText, error: chips.some(c => /fail|error/i.test(`${c.c || ""} ${c.x || ""}`)),
    running: chips.some(c => /running|active/i.test(c.x || "")), duration: chipText,
    html: partsHtml((record.body || []).filter(p => p.p !== "blocks")), raw: record,
    path: head.path || head.att_path,
    revealSig: head.sig || head.att_sig,
    fileSig: head.fsig || head.att_fsig,
    attachment: record.kind === "attachment" ? head : null,
    interaction: head.interaction || null,
    childId: head.child_id || childFrom(head.child), children
  };
}

function childFrom(value) {
  if (!value) return "";
  try { return new URL(value, location.href).searchParams.get("session") || ""; }
  catch (_) { return ""; }
}

export function partsHtml(parts = []) {
  return parts.map(part => {
    if (!part || typeof part !== "object") return `<pre class="fallback-raw">${escapeText(part)}</pre>`;
    if (part.p === "md" || part.p === "think") return part.h || "";
    if (part.p === "pre" || part.p === "raw") return `<pre>${escapeText(part.x || "")}</pre>`;
    if (part.p === "note") return `<div class="renderer-note"><p>${escapeText(part.x || "")}</p></div>`;
    if (part.p === "num" || part.p === "diff") return codeRows(part);
    if (part.p === "blocks") return "";
    return `<div class="renderer-fallback"><div class="renderer-fallback-row"><span>unknown part</span><code>${escapeText(JSON.stringify(part))}</code></div></div>`;
  }).join("");
}

function codeRows(part) {
  const rows = (part.rows || []).map(row => {
    const kind = part.p === "diff" ? row[0] || "" : "";
    const line = part.p === "num" ? row[0] : row[1];
    const code = part.p === "num" ? row[1] : escapeText(row[2] || "");
    return `<div class="line ${escapeText(kind)}"><span class="ln">${escapeText(line ?? "")}</span><span class="mark">${kind === "add" ? "+" : kind === "del" ? "−" : ""}</span><span class="codecell">${code || " "}</span></div>`;
  }).join("");
  return `<div class="codebox" data-codebox><div class="lines wrap">${rows}</div></div>`;
}

export class Projection {
  constructor() { this.units = []; }
  rebuild(records, changedFrom = 0) {
    // Desktop attachments are separate canonical records immediately after their prompt. When
    // one arrives in a later pull, rewind only that owning prompt unit; otherwise an incremental
    // append would briefly project the attachment as a standalone Agent Process.
    if (records[changedFrom]?.kind === "attachment") {
      const owner = [...this.units].reverse().find(unit => unit.type === "user" && unit.to === changedFrom - 1);
      if (owner) changedFrom = owner.from;
    }
    let keep = 0;
    while (keep < this.units.length && this.units[keep].to < changedFrom) keep++;
    if (keep < this.units.length) changedFrom = this.units[keep].from;
    else if (this.units.length) changedFrom = this.units.at(-1).to + 1;
    const prefix = this.units.slice(0, keep);
    const units = buildUnits(records, changedFrom, prefix.at(-1)?.turn || 0);
    this.units = prefix.concat(units);
    return keep;
  }
}

function buildUnits(records, from, turn) {
  const units = [];
  let process = null;
  const flush = () => { if (process) units.push(process); process = null; };
  for (let i = from; i < records.length; i++) {
    const record = records[i];
    const view = viewRecord(record);
    if (record.kind === "user") {
      flush(); turn = Number(record.turn || turn + 1);
      units.push({ type: "user", key: `user:${record.id || i}`, from: i, to: i, turn, view, attachments: [], label: record.label || plainText(record).slice(0, 80) });
    } else if (record.kind === "attachment" && !process && units.at(-1)?.type === "user") {
      const unit = units.at(-1);
      const path = view.attachment?.att_path || "";
      const duplicate = unit.attachments.some(item => path && item.attachment?.att_path === path);
      if (!duplicate) unit.attachments.push(view);
      unit.to = i;
    } else if (record.kind === "assistant" && record.phase !== "commentary") {
      flush(); units.push({ type: "assistant", key: `assistant:${record.id || i}`, from: i, to: i, turn, view });
    } else {
      if (!process) process = { type: "process", key: `process:${record.id || i}`, from: i, to: i, turn, views: [] };
      process.to = i; process.views.push({ index: i, view });
    }
  }
  flush();
  return units;
}
