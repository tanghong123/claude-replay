// The shape of a workflow run's roster (#241): its members grouped by the phase() the script
// declared, for whichever page is drawing them. Markup stays with each page; only the SHAPE is
// shared, because getting the grouping subtly different on the two pages is exactly the class of
// drift seam 0 exists to prevent.
//
// Measured across the 68 runs on this machine: 9 record a phase and 59 do not, and 742 of the
// agents carry none at all. So a flat run must stay flat — a page that invented a phase heading
// for a run that declared none would be adding information the journal never had.

/** Group a run's members by phase, in launch order.
 *
 *  Returns `[{ phase, members }]`. `phase` is null for members the run gave none, and a run
 *  where NO member has a phase comes back as a single null group — i.e. exactly the flat list
 *  the pages drew before, so nothing changes for the 59 runs that never named one.
 *
 *  Phases appear in the order they were first launched, not alphabetically: a workflow's phases
 *  are a sequence (Find, then Verify), and sorting them would scramble the story. Members with
 *  no phase in an otherwise-phased run collect in a trailing null group rather than being hidden
 *  or filed under someone else's heading. */
function fleetGroups(members) {
  const list = Array.isArray(members) ? members : [];
  if (!list.some(m => m && m.phase)) return list.length ? [{ phase: null, members: list }] : [];
  const order = [];
  const byPhase = new Map();
  for (const m of list) {
    const key = (m && m.phase) || null;
    if (!byPhase.has(key)) {
      byPhase.set(key, []);
      order.push(key);
    }
    byPhase.get(key).push(m);
  }
  // The unphased remainder sits last whatever order it was launched in: it is the leftover, and
  // reading it between two named phases would imply it belonged to one of them.
  order.sort((a, b) => (a === null) - (b === null));
  return order.map(phase => ({ phase, members: byPhase.get(phase) }));
}

export { fleetGroups };
