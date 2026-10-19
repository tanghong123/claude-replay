/** Native model capabilities determine both pickers. An effort is never invented by the UI. */
export function modelOptions(catalog) {
  return (Array.isArray(catalog) ? catalog : []).filter(value => value && typeof value.id === "string").map(value => ({
    id: value.id,
    label: value.displayName || value.id,
    model: value.model || value.id,
    default: value.isDefault === true,
    defaultEffort: value.defaultReasoningEffort || "",
    defaultTier: value.defaultServiceTier ?? null,
    tiers: (Array.isArray(value.serviceTiers) ? value.serviceTiers : []).filter(tier => tier && typeof tier.id === "string" && typeof tier.name === "string").map(tier => ({ id: tier.id, name: tier.name, description: tier.description || "" })),
    efforts: (value.supportedReasoningEfforts || []).map(option => typeof option === "string" ? { value: option, label: option } : { value: option.reasoningEffort, label: option.reasoningEffort, description: option.description || "" }).filter(option => typeof option.value === "string" && option.value),
  }));
}

export function selectedCapabilities(models, model, effort) {
  const selected = models.find(option => option.id === model || option.model === model) || models.find(option => option.default) || models[0];
  if (!selected) return { model: "", effort: "", efforts: [] };
  const supported = selected.efforts.some(option => option.value === effort);
  const advertisedDefault = selected.efforts.find(option => option.value === selected.defaultEffort)?.value;
  return { model: selected.id, effort: supported ? effort : advertisedDefault || selected.efforts[0]?.value || "", efforts: selected.efforts };
}

/** Existing tasks display only effective native settings; catalog defaults cannot replace them. */
export function effectiveCapabilities(models, settings = {}) {
  const model = typeof settings?.model === "string" ? settings.model : "";
  const effort = typeof settings?.effort === "string" && settings.effort ? settings.effort : typeof settings?.reasoningEffort === "string" ? settings.reasoningEffort : "";
  const selected = models.find(option => option.id === model || option.model === model);
  const modelAvailable = !!selected;
  const effortAvailable = !!selected?.efforts.some(option => option.value === effort);
  const reason = !models.length ? "The model catalog is unavailable. Reconnect before sending." : !model ? "Codex has not confirmed this task's model. Select a model before sending." : !modelAvailable ? `Saved model ${model} is unavailable. Select an available model before sending.` : !effort ? "Codex has not confirmed this task's reasoning effort. Select an effort before sending." : !effortAvailable ? `Saved effort ${effort} is unavailable for this model. Select an available effort before sending.` : "";
  return { model, modelId: selected?.id || model, canonicalModel: selected?.model || model, effort, efforts: selected?.efforts || [], modelAvailable, effortAvailable, ready: modelAvailable && effortAvailable, reason };
}

/** Safe visible fallback while the existing Rust Markdown renderer is being fetched. */
export const escapeText = text => String(text ?? "").replace(/[&<>"']/g, character => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
export const plainMarkup = text => `<p>${escapeText(text).replace(/\n/g, "<br>")}</p>`;

export function textOfInput(content = []) {
  return content.map(value => {
    if (value?.type === "text") return value.text || "";
    if (value?.type === "image" || value?.type === "localImage") {
      const source = value.path || value.url;
      return typeof source === "string" && !source.startsWith("data:") ? `[Image: ${source}]` : "[Image attached]";
    }
    if (value?.type === "skill") return `[Skill: ${value.name || value.path || ""}]`;
    // The transcript and queue share this text. Retain an unknown native variant's
    // complete content; both Markdown and plain-text consumers escape active HTML.
    return JSON.stringify(value, null, 2) ?? "[Attachment]";
  }).join("\n");
}

/** Native unified patches -> Monitor diff rows, keeping headers and source line numbers. */
export function patchRows(patch) {
  let oldLine = 0, newLine = 0, oldRemaining = 0, newRemaining = 0, inHunk = false, sawHunk = false;
  return String(patch || "").split("\n").map(line => {
    const hunk = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (hunk) {
      oldLine = Number(hunk[1]); oldRemaining = Number(hunk[2] ?? 1); newLine = Number(hunk[3]); newRemaining = Number(hunk[4] ?? 1); inHunk = true; sawHunk = true;
      return ["ctx", null, line];
    }
    if (line.startsWith("\\ No newline")) return ["ctx", null, line];
    let row;
    if (inHunk && line.startsWith("+")) { row = ["add", newLine++, line.slice(1)]; newRemaining--; }
    else if (inHunk && line.startsWith("-")) { row = ["del", oldLine++, line.slice(1)]; oldRemaining--; }
    else if (inHunk && line.startsWith(" ")) { row = ["ctx", newLine++, line.slice(1)]; oldLine++; oldRemaining--; newRemaining--; }
    else if (!sawHunk && !line.startsWith("+++ ") && !line.startsWith("--- ") && /^[+-]/.test(line)) row = [line[0] === "+" ? "add" : "del", null, line.slice(1)];
    else row = ["ctx", null, line];
    if (oldRemaining <= 0 && newRemaining <= 0) inHunk = false;
    return row;
  });
}

/** Match the shared presenter's duration vocabulary; missing and zero stay distinct. */
function commandDuration(milliseconds) {
  if (!Number.isSafeInteger(milliseconds) || milliseconds < 0) return null;
  if (milliseconds === 0) return "0ns";
  if (milliseconds < 1000) return `${milliseconds}ms`;
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds >= 60) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  const centiseconds = Math.floor(milliseconds % 1000 / 10);
  return centiseconds ? `${seconds}.${String(centiseconds).padStart(2, "0")}s` : `${seconds}s`;
}

function commandRecord(record, item, cwd) {
  record.kind = "bash"; record.tool = "command"; record.head.name = "Command"; record.head.target = item.command || "";
  record.body = [{ p: "note", x: `Directory: ${cwd}` }, { p: "pre", x: item.aggregatedOutput || "" }];
  if (item.exitCode != null) record.head.chips.push({ x: `exit ${item.exitCode}`, c: item.exitCode === 0 ? "state" : "err" });
  const duration = commandDuration(item.durationMs);
  if (duration != null) record.head.chips.push({ x: duration, c: "state" });

  // Native classification is authoritative. One unknown or malformed action keeps
  // the whole compound command visible as Bash rather than hiding part of its work.
  const actions = item.commandActions;
  if (!Array.isArray(actions) || !actions.length || actions.some(action => !action || !["read", "search", "listFiles"].includes(action.type) || typeof action.command !== "string")) return;
  const relative = path => typeof path === "string" && path.startsWith(`${cwd.replace(/\/$/, "")}/`) ? path.slice(cwd.replace(/\/$/, "").length + 1) : path;
  const counts = { read: 0, search: 0, listFiles: 0 };
  const readTargets = new Set();
  const children = actions.map((action, index) => {
    const path = typeof action.path === "string" && action.path ? relative(action.path) : "";
    const query = typeof action.query === "string" ? action.query : "";
    const name = action.type === "read" ? "Read" : action.type === "search" ? "Grep" : "LS";
    const target = action.type === "search" ? query ? `${query}${path ? ` in ${path}` : ""}` : action.command : path || action.command;
    if (action.type === "read") { readTargets.add(target); counts.read = readTargets.size; }
    else counts[action.type]++;
    return { id: `${record.id}:action:${index}`, turn: record.turn, src: "", kind: action.type === "read" ? "read" : "tool", tool: name, runtime: { ...record.runtime }, head: { name, target, chips: [] }, body: [] };
  });
  // As in canonical exploration spans, the final action owns the exact compound
  // command/output and execution facts; it adds no fictitious shell action.
  const last = children.at(-1);
  last.head.chips = record.head.chips.map(chip => ({ ...chip }));
  last.body = [{ p: "pre", x: `$ ${item.command || ""}` }, ...record.body];
  const summary = [
    counts.search ? `searched for ${counts.search} pattern${counts.search === 1 ? "" : "s"}` : "",
    counts.read ? `read ${counts.read} file${counts.read === 1 ? "" : "s"}` : "",
    counts.listFiles ? `listed ${counts.listFiles} director${counts.listFiles === 1 ? "y" : "ies"}` : "",
  ].filter(Boolean).join(", ");
  record.kind = "act"; delete record.tool;
  record.head.name = "Activity"; delete record.head.target;
  record.head.summary = summary[0].toUpperCase() + summary.slice(1);
  record.body = [{ p: "blocks", items: children }];
}

/** Native thread items -> Monitor's normalized records. Monitor owns projection and rendering. */
export function threadRecords(thread, markdown = plainMarkup, planUpdates = []) {
  const records = [];
  for (const [turnIndex, turn] of (thread?.turns || []).entries()) {
    // Legacy full reads may precede Core completion notifications. Only saved
    // aliases belong in the transcript; provisional Core IDs cannot be reconciled.
    for (const [itemIndex, item] of (turn.items || []).entries()) {
      const id = item.id || `${turn.id}:${itemIndex}`;
      const nativeStatus = item.status || (turn.status === "inProgress" ? "running" : "completed");
      const status = nativeStatus === "inProgress" || nativeStatus === "in_progress" ? "running" : nativeStatus;
      const record = { id, turn: turnIndex + 1, src: "", kind: "tool", runtime: { running: status === "running" }, head: { name: item.type || "Item", chips: [{ x: status, c: "state" }] }, body: [] };
      switch (item.type) {
        case "userMessage":
          record.kind = "user"; record.src = textOfInput(item.content); record.body = [{ p: "md", h: markdown(record.src) }]; break;
        case "agentMessage":
          record.kind = "assistant"; record.phase = item.phase === "commentary" ? "commentary" : item.phase === "final_answer" || item.phase === "final" ? "final" : "unknown"; record.src = item.text || ""; record.body = [{ p: "md", h: markdown(record.src) }];
          if (item.partialFromJoin) record.body.push({ p: "note", x: "Joined while Codex was replying. The complete message appears when this reply finishes." });
          break;
        case "reasoning":
          record.kind = "think"; record.src = [...(item.summary || []), ...(item.content || [])].map(value => typeof value === "string" ? value : value.text || "").join("\n"); record.body = [{ p: "think", h: markdown(record.src) }];
          if (item.partialFromJoin) record.body.push({ p: "note", x: "Joined while Codex was reasoning. Only subsequently received reasoning is shown until history refreshes." });
          break;
        case "commandExecution":
          commandRecord(record, item, item.cwd || thread.cwd || "unknown"); break;
        case "fileChange":
          record.kind = "edit"; record.tool = "fileChange"; record.head.name = "File changes";
          record.head.target = (item.changes || []).map(change => change.kind?.move_path ? `${change.path} (moved to ${change.kind.move_path})` : change.path).join(", ");
          record.body = (item.changes || []).flatMap(change => [
            { p: "note", x: change.path },
            ...(["add", "delete"].includes(change.kind?.type) ? [{ p: "note", x: change.kind.type === "add" ? "Added file" : "Deleted file" }] : []),
            ...(change.kind?.move_path ? [{ p: "note", x: `Moved to: ${change.kind.move_path}` }] : []),
            ...(change.kind != null && !["add", "delete", "update"].includes(change.kind.type) ? [{ p: "pre", x: JSON.stringify(change.kind, null, 2) }] : []),
            { p: "diff", rows: patchRows(change.diff) },
          ]);
          if (typeof item.aggregatedOutput === "string" && item.aggregatedOutput) record.body.push({ p: "pre", x: item.aggregatedOutput });
          break;
        case "dynamicToolCall":
          record.kind = "tool"; record.tool = item.tool || "dynamicTool"; record.head.name = item.tool || "Desktop tool"; record.head.target = "Handled by the Codex desktop client"; record.body = [{ p: "pre", x: JSON.stringify({ arguments: item.arguments, output: item.contentItems }, null, 2) }]; break;
        case "mcpToolCall":
          record.tool = item.tool || "mcp"; record.head.name = `${item.server || "MCP"} · ${item.tool || "Tool"}`; record.body = [{ p: "pre", x: JSON.stringify({ arguments: item.arguments, result: item.result, error: item.error }, null, 2) }]; break;
        case "plan":
          record.kind = "assistant"; record.phase = "final"; record.head.presentation = "proposed_plan"; record.src = item.text || ""; record.body = [{ p: "md", h: markdown(record.src) }]; break;
        case "webSearch":
          record.head.name = "Web search"; record.head.target = item.query || item.action?.query || ""; record.body = [{ p: "pre", x: JSON.stringify(item.action || {}, null, 2) }];
          if (item.results != null) record.body.push({ p: "pre", x: JSON.stringify(item.results, null, 2) });
          break;
        case "contextCompaction":
          record.kind = "compaction"; record.head.name = "Context compacted"; break;
        default:
          record.body = [{ p: "pre", x: JSON.stringify(item, null, 2) }];
      }
      records.push(record);
    }
    const plan = planUpdates.find(update => update.threadId === thread.id && update.turnId === turn.id);
    if (plan) {
      const status = { pending: "Pending", inProgress: "InProgress", completed: "Completed" };
      const lines = ["Updated Plan", ...(typeof plan.explanation === "string" ? [plan.explanation] : []), ...plan.plan.map(item => `${status[item.status]}: ${item.step}`)];
      const extra = Object.fromEntries(Object.entries(plan).filter(([key]) => !["threadId", "turnId", "explanation", "plan"].includes(key)));
      // Live plan notifications are separate from replayable native ThreadItems.
      // Use the existing TodoWrite reader without fabricating a native item.
      records.push({ id: `${turn.id}:plan-update`, turn: turnIndex + 1, kind: "tool", tool: "TodoWrite", src: "", runtime: { running: false }, head: { name: "TodoWrite", chips: [] }, body: [{ p: "pre", x: lines.join("\n") }, ...(Object.keys(extra).length ? [{ p: "pre", x: JSON.stringify(extra, null, 2) }] : [])], nativeSource: plan });
    }
    // Persisted failed turns must keep their native error readable after reload,
    // even when the browser missed the transient error notification.
    if (turn.error) {
      const message = typeof turn.error === "string" ? turn.error : turn.error.message || "Codex task failed";
      records.push({ id: `${turn.id}:error`, turn: turnIndex + 1, kind: "tool", src: "", runtime: { running: false }, head: { name: "Codex error", target: message, chips: [{ x: "failed", c: "err" }] }, body: [{ p: "pre", x: typeof turn.error === "string" ? turn.error : JSON.stringify(turn.error, null, 2) }] });
    }
  }
  return records;
}

export function activeTurn(thread) {
  return [...(thread?.turns || [])].reverse().find(turn => turn.status === "inProgress" || turn.status === "in_progress") || null;
}

export function freshWorkspace() {
  return { thread: null, planUpdates: [], provisionalItems: [], legacyHistoryDirty: false, accessMode: "active", settings: {}, pending: [], queue: [], connection: "connecting", epoch: null, sequence: 0, needsSnapshot: false };
}

const validPlanUpdate = (value, threadId) => value && typeof threadId === "string" && threadId.length > 0 && value.threadId === threadId && typeof value.turnId === "string" && value.turnId.length > 0 && Array.isArray(value.plan) && (value.explanation == null || typeof value.explanation === "string") && value.plan.every(step => step && typeof step.step === "string" && ["pending", "inProgress", "completed"].includes(step.status));

export function applySnapshot(state, snapshot) {
  const record = value => value && typeof value === "object" && !Array.isArray(value);
  const id = value => typeof value === "string" && value.length > 0;
  if (!record(snapshot) || !record(snapshot.thread) || !id(snapshot.thread.id) || !Array.isArray(snapshot.thread.turns)) throw new Error("The server returned an invalid thread snapshot");
  if (!snapshot.thread.turns.every(turn => record(turn) && id(turn.id) && Array.isArray(turn.items)
    && turn.items.every(item => record(item) && id(item.id) && id(item.type)))) throw new Error("The server returned invalid turn or item structures");
  for (const field of ["pending", "queue"]) if (snapshot[field] !== undefined && (!Array.isArray(snapshot[field]) || !snapshot[field].every(record))) throw new Error(`The server returned an invalid ${field} list`);
  if (snapshot.settings !== undefined && !record(snapshot.settings)) throw new Error("The server returned invalid thread settings");
  const accessMode = snapshot.accessMode === undefined ? "active" : snapshot.accessMode;
  if (!["active", "archived"].includes(accessMode)) throw new Error("The server returned an invalid thread access mode");
  const planUpdates = snapshot.planUpdates === undefined ? [] : snapshot.planUpdates;
  if (!Array.isArray(planUpdates) || !planUpdates.every(value => validPlanUpdate(value, snapshot.thread.id))) throw new Error("The server returned invalid native plan updates");
  const legacyHistoryDirty = snapshot.legacyHistoryDirty === undefined ? false : snapshot.legacyHistoryDirty;
  if (typeof legacyHistoryDirty !== "boolean" || legacyHistoryDirty && (snapshot.thread.historyMode !== "legacy" || accessMode !== "active")) throw new Error("The server returned invalid legacy history synchronization state");
  const provisionalItems = snapshot.provisionalItems === undefined ? [] : snapshot.provisionalItems;
  const provisionalKeys = new Set();
  if (!Array.isArray(provisionalItems) || !provisionalItems.every(value => {
    const turn = snapshot.thread.turns.find(turn => turn.id === value?.turnId);
    const item = value?.item, key = JSON.stringify([value?.turnId, item?.id]);
    const valid = accessMode === "active" && snapshot.thread.historyMode === "legacy" && turn?.status === "inProgress"
      && typeof item?.id === "string" && item.id.length > 0 && ["agentMessage", "reasoning"].includes(item.type)
      && (item.partialFromJoin === undefined || typeof item.partialFromJoin === "boolean")
      && (item.type === "agentMessage" ? typeof item.text === "string" : [item.summary, item.content].every(parts => Array.isArray(parts) && parts.every(part => typeof part === "string")))
      && !provisionalKeys.has(key);
    provisionalKeys.add(key); return valid;
  })) throw new Error("The server returned invalid provisional legacy items");
  if (snapshot.historySource != null && (accessMode !== "archived" || snapshot.historySource.kind !== "nativeArchivedRollout" || snapshot.historySource.historyMode !== "paginated" || !Number.isSafeInteger(snapshot.historySource.bytes) || snapshot.historySource.bytes < 0)) throw new Error("The server returned invalid archived history provenance");
  return { ...state, ...snapshot, historySource: accessMode === "archived" ? snapshot.historySource || null : null, planUpdates, provisionalItems, legacyHistoryDirty, accessMode, pending: accessMode === "archived" ? [] : snapshot.pending || [], queue: accessMode === "archived" ? [] : snapshot.queue || [], settings: accessMode === "archived" ? {} : snapshot.settings || {}, needsSnapshot: false, lastError: null };
}

const sameId = (left, right) => typeof left === typeof right && left === right;
const findItem = (thread, params) => {
  const turn = thread?.turns?.find(value => value.id === params.turnId);
  return turn?.items?.find(value => value.id === params.itemId);
};

/** A late subscriber may miss item/started: show only the suffix actually received. */
const findStreamingItem = (thread, params, type) => {
  const existing = findItem(thread, params);
  if (existing) return existing;
  const turn = thread?.turns?.find(value => value.id === params.turnId);
  if (!turn || !["inProgress", "in_progress"].includes(turn.status) || typeof params.itemId !== "string" || typeof params.delta !== "string") return null;
  const item = { id: params.itemId, type, text: "", ...(type === "agentMessage" ? { phase: null } : {}), partialFromJoin: true };
  turn.items ||= [];
  turn.items.push(item);
  return item;
};

const legacyAliasType = type => ["userMessage", "agentMessage", "reasoning", "contextCompaction"].includes(type);

/** Unfinished legacy messages have actual live IDs, separate from persisted aliases. */
function legacyStreamingItem(state, params, type) {
  if (state.accessMode !== "active") return null;
  const turn = state.thread?.turns.find(turn => turn.id === params.turnId);
  if (turn?.status !== "inProgress" || typeof params.itemId !== "string" || !params.itemId) return null;
  const existing = state.provisionalItems.find(value => value.turnId === params.turnId && value.item.id === params.itemId);
  if (existing) return existing.item;
  const item = type === "reasoning" ? { id: params.itemId, type, summary: [], content: [], partialFromJoin: true }
    : { id: params.itemId, type, text: "", phase: null, partialFromJoin: true };
  state.provisionalItems.push({ turnId: params.turnId, item }); return item;
}

/** SSE sequence gaps require an authoritative snapshot, never guessed transcript state. */
export function reduceEvent(state, event) {
  if (!event || !Number.isInteger(event.sequence) || event.sequence <= state.sequence && event.epoch === state.epoch) return state;
  if (state.epoch != null && (event.epoch !== state.epoch || event.sequence !== state.sequence + 1)) return { ...state, pending: [], provisionalItems: [], ...(event.epoch !== state.epoch ? { planUpdates: [] } : {}), sequence: event.sequence, epoch: event.epoch, needsSnapshot: true, ...(event.method === "gateway/connection" ? { connection: event.params?.state || event.params?.connection || "reconnecting" } : {}) };
  const next = structuredClone({ ...state, epoch: event.epoch, sequence: event.sequence });
  const params = event.params || {};
  if (event.method === "gateway/connection") {
    next.connection = params.state || params.connection || "reconnecting";
    next.pending = []; next.provisionalItems = []; next.needsSnapshot = true;
    return next;
  }
  if (params.threadId && params.threadId !== state.thread?.id) return next;
  if (event.method === "thread/archived" || event.method === "gateway/snapshot-required" && params.accessMode === "archived") {
    next.accessMode = "archived"; next.pending = []; next.provisionalItems = []; next.queue = []; next.settings = {};
    if (event.method === "gateway/snapshot-required") next.needsSnapshot = true;
    return next;
  }
  if (event.method === "thread/unarchived") {
    // Native unarchive moves the history file. A fresh resume must prove that
    // this browser's execution session is attached before writers are enabled.
    next.needsSnapshot = true;
    return next;
  }
  if (event.method === "gateway/recoveryError") {
    next.thread = null; next.pending = []; next.queue = []; next.planUpdates = []; next.provisionalItems = [];
    next.lastError = params.message || params.error?.message || "This task could not be recovered from the shared runtime";
    return next;
  }
  if (event.method === "serverRequest/resolved") {
    next.pending = next.pending.filter(request => !sameId(request.id, params.requestId)); return next;
  }
  if (event.method === "gateway/legacy-history-changed" && next.thread?.historyMode === "legacy") {
    if (next.accessMode === "archived") return next;
    if (typeof params.turnId !== "string" || !params.turnId || typeof params.itemId !== "string" || !params.itemId || !["itemStarted", "itemCompleted"].includes(params.reason)) return next;
    if (params.reason === "itemCompleted") next.provisionalItems = next.provisionalItems.filter(value => value.turnId !== params.turnId || value.item.id !== params.itemId);
    // This is a display refresh, not a missing request/scope proof. Stop and
    // pending native controls remain usable while legacy persistence catches up.
    next.legacyHistoryDirty = true; return next;
  }
  if (event.method === "gateway/request" || event.requestId != null || event.id != null || event.request) {
    if (next.accessMode === "archived") return next;
    const request = event.request || params.request || { id: event.requestId ?? event.id ?? params.id, method: params.method || event.method, params: params.params || params, canRespond: params.canRespond ?? !/tool\/call$/.test(event.method) };
    if (request.id != null) next.pending = [...next.pending.filter(value => !sameId(value.id, request.id)), request];
    return next;
  }
  if (/requestApproval$|requestUserInput$|^item\/tool\/call$/.test(event.method) && params.requestId != null) {
    if (next.accessMode === "archived") return next;
    next.pending.push({ id: params.requestId, method: event.method, params, canRespond: !/tool\/call$/.test(event.method) }); return next;
  }
  if (event.method === "thread/settings/updated") {
    if (next.accessMode === "archived") return next;
    next.settings = params.threadSettings || params.settings || next.settings;
    if (next.thread && typeof next.settings.cwd === "string") next.thread.cwd = next.settings.cwd;
  }
  else if (event.method === "turn/plan/updated" && validPlanUpdate(params, next.thread?.id)) {
    const knownTurn = next.thread.turns.some(turn => turn.id === params.turnId);
    next.planUpdates = (next.planUpdates || []).filter(update => update.turnId !== params.turnId && (knownTurn || next.thread.turns.some(turn => turn.id === update.turnId)));
    next.planUpdates.push(params);
    if (!knownTurn) next.needsSnapshot = true;
  }
  else if (event.method === "thread/status/changed" && next.thread) next.thread.status = params.status;
  else if (event.method === "turn/started" && next.thread) {
    next.thread.turns ||= [];
    if (!next.thread.turns.some(turn => turn.id === params.turn.id)) next.thread.turns.push({ ...params.turn, items: next.thread.historyMode === "legacy" ? [] : params.turn.items || [] });
    // An early plan can have been retained by the Gateway while an intervening
    // snapshot still omitted its unconfirmed turn. Fetch the now-confirmed plan.
    next.needsSnapshot = true;
  } else if (event.method === "turn/completed" && next.thread) {
    const turn = next.thread.turns?.find(value => value.id === params.turn.id);
    if (turn) Object.assign(turn, params.turn, { items: next.thread.historyMode === "legacy" ? turn.items : params.turn.items?.length ? params.turn.items : turn.items });
    next.provisionalItems = next.provisionalItems.filter(value => value.turnId !== params.turn.id);
    next.needsSnapshot = true;
  } else if (/^item\/(started|completed)$/.test(event.method) && next.thread) {
    if (next.thread.historyMode === "legacy" && legacyAliasType(params.item.type)) {
      if (next.accessMode === "archived") return next;
      if (event.method === "item/completed") {
        next.provisionalItems = next.provisionalItems.filter(value => value.turnId !== params.turnId || value.item.id !== params.item.id);
        next.legacyHistoryDirty = true;
      } else if (["agentMessage", "reasoning"].includes(params.item.type)) {
        const item = legacyStreamingItem(next, { ...params, itemId: params.item.id }, params.item.type);
        if (item) Object.assign(item, params.item, { partialFromJoin: false });
      } else next.legacyHistoryDirty = true;
      return next;
    }
    const turn = next.thread.turns?.find(value => value.id === params.turnId);
    if (turn) {
      const index = turn.items.findIndex(value => value.id === params.item.id);
      if (index < 0) turn.items.push(params.item); else turn.items[index] = params.item;
    }
    if (params.item.type === "dynamicToolCall" && event.method === "item/completed") next.pending = next.pending.filter(request => request.params?.callId !== params.item.id && request.params?.itemId !== params.item.id);
  } else if (/^item\/(agentMessage|plan)\/delta$/.test(event.method)) {
    const type = event.method.includes("agentMessage") ? "agentMessage" : "plan";
    const item = next.thread?.historyMode === "legacy" && type === "agentMessage" ? legacyStreamingItem(next, params, type) : findStreamingItem(next.thread, params, type);
    if (item && typeof params.delta === "string") item.text = (item.text || "") + params.delta;
  } else if (event.method === "item/commandExecution/outputDelta" || event.method === "item/fileChange/outputDelta") {
    const item = findItem(next.thread, params); if (item) item.aggregatedOutput = (item.aggregatedOutput || "") + (params.delta || "");
  } else if (event.method === "item/fileChange/patchUpdated") {
    const item = findItem(next.thread, params); if (item && Array.isArray(params.changes)) item.changes = params.changes;
  } else if (/^item\/reasoning\/(summaryTextDelta|textDelta|summaryPartAdded)$/.test(event.method)) {
    const item = next.thread?.historyMode === "legacy" ? legacyStreamingItem(next, params, "reasoning") : findItem(next.thread, params);
    const index = params.summaryIndex ?? params.contentIndex ?? 0;
    if (item && Number.isSafeInteger(index) && index >= 0 && index <= 4096 && (event.method.endsWith("PartAdded") || typeof params.delta === "string")) {
      const field = event.method.includes("summary") ? "summary" : "content"; item[field] ||= [];
      while (item[field].length <= index) item[field].push("");
      if (typeof params.delta === "string") item[field][index] += params.delta;
    }
  } else if (/^thread\/queue\//.test(event.method) || event.method === "gateway/snapshot-required") next.needsSnapshot = true;
  else if (event.method === "error") next.lastError = params.error?.message || params.message || "Codex reported an error";
  return next;
}
