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

/**
 * Claude desktop side sessions nested under their starter (#s340): a family whose representative
 * names a starter (`sideOf`, the server's, read from the Claude desktop app's own records) that is
 * a row of another family in `fams`, in the same project, moves under that family, into its `sides`
 * in the order given. The merged group takes the place of its earliest member, so a side session
 * that is active or waits on the person lifts its starter with it rather than sinking to the
 * starter's place (the review of #s340: behind "Show N more" otherwise). A side session whose
 * starter is not among `fams` (the session filter or a hide left it out) or is in another project
 * stays a family of its own, so it is never lost or shown away from its project; a starter named in
 * a loop nests nothing; a side session of a side session nests under that one.
 */
export function nestSideSessions(fams = []) {
  const owner = new Map();
  for (const fam of fams) for (const row of fam.members) owner.set(row.id, fam);
  const starterOf = fam => {
    const starter = fam.rep.sideOf && owner.get(fam.rep.sideOf);
    if (!starter || starter === fam) return null;
    if (starter.project && fam.project && starter.project.id !== fam.project.id) return null;
    return starter;
  };
  // A family nests only where its starters lead to a family that names none; a loop nests nothing.
  const nests = fam => {
    const seen = new Set([fam]);
    for (let at = starterOf(fam); at; at = starterOf(at)) {
      if (seen.has(at)) return false;
      seen.add(at);
    }
    return Boolean(starterOf(fam));
  };
  const out = fams.map(fam => ({ ...fam, sides: [] }));
  const copy = new Map(fams.map((fam, i) => [fam, out[i]]));
  const nested = new Set();
  fams.forEach((fam, i) => {
    if (!nests(fam)) return;
    copy.get(starterOf(fam)).sides.push(out[i]);
    nested.add(out[i]);
  });
  const place = new Map(out.map((fam, i) => [fam, i]));
  const earliest = fam => Math.min(place.get(fam), ...fam.sides.map(earliest));
  return out.filter(fam => !nested.has(fam)).sort((a, b) => earliest(a) - earliest(b));
}

/** Whether a family, its forks or a side session under it holds the row `id` (#s340). */
export function familyHolds(fam, id) {
  return fam.members.some(row => row.id === id) || (fam.sides || []).some(side => familyHolds(side, id));
}
