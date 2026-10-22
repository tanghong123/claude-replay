// The app shell's session list as ONE list (#s206). Pure, and importing only the shared grouping,
// so the contract test runs it under node. It is the app shell's alone: the classic page has no
// flat list, so it stays out of the shared modules every exported page inlines.
import { families } from "./shared/session-visibility.js";

/** How long a session counts as active for the list's ORDER — the server's `ACTIVE_WINDOW`
 *  (claude-monitor/src/index.rs, which a test holds to this), not the filter's hour. */
export const CALM_SECS = 10 * 60;

/**
 * The tree flattened into one list (the owner: "organize the sessions simply by active time (not
 * by projects or agents)"): every family the tree would show, each carrying its `agent` and
 * `project`, in the server's CALM order (owner, 2026-08-08: rows must not hop with an agent's
 * bursty writes). The families active in the last ten minutes come first, tied and in name order;
 * the rest follow by their newest activity; a stable key settles every tie, so two polls of the
 * same data give the same list. Families are formed within a project, as the tree forms them, so
 * a family is the same one in both views. `tree` is `visibleTree`'s output.
 */
export function flatFamilies(tree = [], now = Date.now() / 1000) {
  const out = [];
  for (const agent of tree) for (const project of agent.projects) {
    for (const family of families(project.rows || [])) out.push({ ...family, agent, project });
  }
  const active = family => now - (family.latest || 0) < CALM_SECS;
  const name = family => String(family.rep.name || family.rep.id || "").toLowerCase();
  const key = family => `${family.agent.id}\u0000${family.project.id}\u0000${family.key}`;
  return out.sort((a, b) => {
    const tier = Number(active(b)) - Number(active(a));
    if (tier) return tier;
    const when = active(a) ? 0 : (b.latest || 0) - (a.latest || 0);
    if (when) return when;
    return name(a).localeCompare(name(b)) || (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0);
  });
}
