// shared: runtime — the runtime snapshot's rows, phrased ONCE for every page (#62).
//
// The wire's `usage.runtime` carries the latest facts an agent's transcript recorded about the
// process that wrote it — context, effort, sandbox, permission mode, … — and `recorded`, the
// keys that agent's FORMAT records at all. A missing value therefore means one of two things,
// and a reader deserves to know which: "unknown" (this agent records it; the transcript has
// not said yet) or "not recorded by this agent" (its format has no such fact — Claude Code has
// no sandbox, Codex names no client version). No page keeps a per-agent table: the adapter
// declared it, the wire carried it, this helper phrases it.

/** Every row the snapshot can show, in display order: [wire key, label]. */
const RUNTIME_KEYS = [
  ["context", "context"],
  ["effort", "effort"],
  ["mode", "mode"],
  ["sandbox", "sandbox"],
  ["approvals", "approvals"],
  ["permission", "permission"],
  ["tier", "service tier"],
  ["plan", "plan"],
  ["client", "client"],
];

/** The four facts a pane shows even when the agent does not record them. */
const RUNTIME_ALWAYS = ["context", "effort", "sandbox", "permission"];

function contextText(rt) {
  if (rt.context_left != null) return rt.context_left + "% left";
  if (rt.context_used_tokens && rt.context_window_tokens) return rt.context_used_tokens + " / " + rt.context_window_tokens;
  return null;
}

/**
 * The rows of a snapshot: `{ key, label, value, state }` where `state` is "value", "unknown"
 * (recorded by this agent, not seen) or "absent" (not recorded by this agent). A null or
 * missing snapshot yields every row absent — nothing was declared.
 */
function runtimeRows(rt) {
  const r = rt || {};
  const recorded = new Set(Array.isArray(r.recorded) ? r.recorded : []);
  return RUNTIME_KEYS.map(([key, label]) => {
    const raw = key === "context" ? contextText(r) : r[key];
    const value = raw == null || raw === "" ? null : String(raw);
    const state = value != null ? "value" : recorded.has(key) ? "unknown" : "absent";
    return { key, label, value, state };
  });
}

/** What a row SAYS, given the agent's display name for the absent case. */
function runtimeText(row, agent) {
  if (row.state === "value") return row.value;
  if (row.state === "unknown") return "unknown";
  return "not recorded by " + (agent || "this agent");
}

export { RUNTIME_ALWAYS, RUNTIME_KEYS, runtimeRows, runtimeText };
