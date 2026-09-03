// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
//
// ONE table from the tracker's verdict (`agentState`: busy / wait / idle, `stateReason`: the
// reasons claude-replay-engine's StateReason emits) to the words a person reads — the chip and
// the info pane in the app shell, the row tooltip in the rail and the splice — so every shell
// says the same thing about the same row (#44, design/monitor-shell-duplication.md §1(f)). A
// row from an index that has not derived a verdict yet carries only the legacy `state`
// (growing / idle / finished); the fallbacks below read it the way every shell always did.

/** Labels by coarse state — the fallback when a reason has no wording of its own. */
const STATE_LABELS = { busy: "Running", wait: "Needs you", idle: "Idle" };

/**
 * Labels by reason. Every StateReason the engine emits has a row here; the monitor's
 * `every_tracker_reason_has_a_label` test holds this table to the Rust enum.
 */
const REASON_LABELS = {
  permission: "Awaiting permission",
  question: "Awaiting an answer",
  "plan-approval": "Awaiting approval",
  "ended-question": "Awaiting a reply",
  "queued-prompt": "Prompt queued",
  done: "Done",
  exited: "Done",
  error: "Failed",
  stalled: "Stalled",
  "exited-mid-work": "Exited abnormally",
  thinking: "Thinking",
  tool: "Running a tool",
  starting: "Starting"
};

/** The reasons the table knows — the contract test enumerates the engine's list against it. */
const REASONS = Object.keys(REASON_LABELS);

/** The verdict a row displays: `{ state, reason, label }`, legacy `state` folded in. */
function displayState(row) {
  const state = row.agentState || (row.state === "growing" ? "busy" : "idle");
  const reason = row.stateReason || (row.state === "finished" ? "exited" : row.state);
  return { state, reason, label: REASON_LABELS[reason] || STATE_LABELS[state] || reason };
}

/** Whether the row needs a person: a wait state, or a reason that ended the agent's own work. */
function needsPerson(row) {
  const s = displayState(row);
  return s.state === "wait" || ["question", "ended-question", "error", "stalled", "exited-mid-work", "permission", "plan-approval"].includes(s.reason);
}

/**
 * The marker a session row carries — `{ label, tone }` — or null when there is nothing to
 * say: running, waiting (tone `wait`, `wait inferred` when the tracker only inferred it),
 * a reply owed, a failure, else "New result" when the row moved past `lastRead` (the
 * viewer's own read mark, epoch seconds; 0 = never read).
 */
function denoteState(row, lastRead = 0) {
  const s = displayState(row);
  if (s.state === "busy") return { label: STATE_LABELS.busy, tone: "busy" };
  if (s.state === "wait") return { label: s.label, tone: `wait${row.stateConfidence === "inferred" ? " inferred" : ""}` };
  if (["question", "ended-question"].includes(s.reason)) return { label: "Awaiting reply", tone: "attention" };
  if (["error", "stalled", "exited-mid-work"].includes(s.reason)) return { label: s.label, tone: "danger" };
  if ((row.activityTs || 0) > Number(lastRead || 0)) return { label: "New result", tone: "unread" };
  return null;
}

/**
 * The row tooltip: the label, then the EVIDENCE for it (#145) — an idle row linked by cwd is
 * the common case, most agents being launched without a session id, so the tooltip says what
 * is actually known rather than just "unconfirmed"; when the directory holds several sessions
 * the count is the size of the doubt (`--resume` offers a picker, so the live agent may be
 * driving any of them).
 */
function stateTip(row) {
  const evidence = row.state === "growing" ? "the transcript grew since the last scan"
    : row.state === "idle" ? (row.conf === "unconfirmed"
      ? (row.ambig > 1
        ? `a live agent is in this directory — but it was started without a session id, and \`--resume\` picks from a list, so it may be driving any of these ${row.ambig} sessions`
        : "alive — the only session in this directory, and a live agent is in it")
      : "alive — a live process names this session")
    : "no growth, no process";
  let tip = `${displayState(row).label} — ${evidence}`;
  if (!row.visited) tip += "  ·  counters appear after first open (lazy fold)";
  return tip;
}

export { STATE_LABELS, REASON_LABELS, REASONS, displayState, needsPerson, denoteState, stateTip };
