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
  error: "Tool failed",
  failed: "Turn failed",
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

/**
 * The three buckets a reader sorts the list by (#202) — a PARTITION of the tracker's vocabulary,
 * every row in exactly one, keyed by the reason the row displays (a `wait` state always carries
 * a wait reason, so the reason decides; the state is the fallback for a reason this table has
 * never heard of):
 *
 *   active  — busy: more progress is coming without anyone — thinking, running a tool, starting,
 *             a queued prompt about to run;
 *   blocked — nothing more happens until a person acts: a WAIT state (a modal — permission, a
 *             question dialog, plan approval), or an idle reason that cut the agent's own work
 *             short — a turn that ended with a question, a failure, a stall, an exit mid-work;
 *   idle    — finished with nothing owed: the turn ended with an answer (`done`), or the process
 *             is gone (`exited`).
 *
 * "Needs attention" IS the blocked bucket — `needsPerson` below is that test and nothing else.
 * A session whose turn simply ended is idle even though its agent now waits for the next
 * prompt: that is the reader's move, not the agent's need, and `denoteState` marks such a row
 * "New result" until the reader opens it. The monitor's `every_tracker_reason_has_a_label` test
 * holds this table to the Rust enum as it holds the labels.
 */
const REASON_BUCKETS = {
  permission: "blocked",
  question: "blocked",
  "plan-approval": "blocked",
  "ended-question": "blocked",
  error: "blocked",
  failed: "blocked",
  stalled: "blocked",
  "exited-mid-work": "blocked",
  "queued-prompt": "active",
  thinking: "active",
  tool: "active",
  starting: "active",
  done: "idle",
  exited: "idle"
};

/** The bucket names, in the order a filter lists them. */
const BUCKETS = ["active", "blocked", "idle"];

/** Which bucket the row is in: `active`, `blocked` or `idle`. */
function sessionBucket(row) {
  const s = displayState(row);
  return REASON_BUCKETS[s.reason] || (s.state === "busy" ? "active" : s.state === "wait" ? "blocked" : "idle");
}

/** Whether the row needs a person — the blocked bucket, by the one definition above. */
function needsPerson(row) {
  return sessionBucket(row) === "blocked";
}

/**
 * What the Blocked checkbox selects, in the words of the predicate — the ONE text the control
 * shows (its title), so it says exactly what `needsPerson` tests.
 */
const BLOCKED_SUMMARY = "Blocked sessions — waiting on you for a permission, an answer or a plan approval, or stopped short by a failure, a stall or an exit mid-work";

/** An hour: how far back "Active recently" reaches (the owner's word, #202). */
const RECENT_SECS = 3600;

/** The session filter's three checkboxes, in the order the sheet lists them. */
const FILTER_BUCKETS = ["recent", "blocked", "idle"];
const FILTER_LABELS = { recent: "Active recently", blocked: "Blocked", idle: "Idle" };

/**
 * Which of the filter's buckets a row is in — one or two (#202): `recent` when the row is busy
 * or its last activity (`activityTs`, epoch seconds) is within the hour; `blocked` when it
 * needs a person (`needsPerson`); `idle` when neither. Recent and blocked overlap on purpose —
 * the owner's "Active recently" is everything with activity in the last hour, a blocked
 * session among them — so the three COVER the rows rather than partition them; the state
 * partition (`sessionBucket`) is what a chip and the design's table read.
 */
function sessionFilterBuckets(row, now = Date.now() / 1000) {
  const out = [];
  if (displayState(row).state === "busy" || (Number(row.activityTs) || 0) > now - RECENT_SECS) out.push("recent");
  if (needsPerson(row)) out.push("blocked");
  if (!out.length) out.push("idle");
  return out;
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

export { STATE_LABELS, REASON_LABELS, REASONS, REASON_BUCKETS, BUCKETS, BLOCKED_SUMMARY, RECENT_SECS, FILTER_BUCKETS, FILTER_LABELS, displayState, sessionBucket, sessionFilterBuckets, needsPerson, denoteState, stateTip };
