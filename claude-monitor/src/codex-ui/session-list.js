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

/** The apps whose session list the list can follow (#s387), as `api/layout` names them. */
export const ARRANGE_SOURCES = ["claude-app", "codex-app", "herdr"];

/**
 * Another app's session list over this monitor's rows (#s387, design/layout-sync.md): each section
 * of `layout` (the server's `api/layout` answer), its entries resolved to rows by id. The list
 * shows the SAME sessions the app shows (#s387: not only the layout, the same sessions), so the
 * session filter and the hides drop no entry, and an entry the monitor has no
 * row for is kept with the app's title (`row: null`), so nothing the app lists goes missing. Each
 * session appears once: the first entry naming it wins, and a repeat is dropped, its children with
 * it (they were placed under the first). `rowsById` maps a row id to its row. Returns the sections
 * that hold anything — each `{ id, label, collapsed, nodes, groups: [{ id, label, collapsed, nodes }] }`,
 * a node being `{ id, row, title, children }` — and `placed`, the ids shown. A group the app draws
 * even when empty (`keep`: a Codex project folder) is kept empty.
 */
export function arrangeSessions(layout, rowsById = new Map(), rowsByPane = new Map()) {
  const placed = new Set();
  const node = entry => {
    const id = String(entry?.id || "");
    // A Herdr pane's row is the one the monitor itself links to that pane, when it links one: the
    // id the pane's hook reported at the agent's start goes stale when the session moves (#s294).
    const row = (entry?.pane && rowsByPane.get(entry.pane)) || (id && rowsById.get(id)) || null;
    if (!id || placed.has(id) || (row && placed.has(row.id))) return null;
    // A headless worker lives in its starter's Agents pane, never in the list (#s53).
    if (row?.startedBy) return null;
    placed.add(id); if (row) placed.add(row.id);
    const children = (entry.children || []).map(node).filter(Boolean);
    return { id: row ? row.id : id, row, title: entry.title || null, children };
  };
  const nodes = entries => (entries || []).map(node).filter(Boolean);
  const sections = [];
  for (const section of layout?.sections || []) {
    const top = nodes(section.entries);
    const groups = (section.groups || [])
      .map(group => ({ id: String(group.id), label: String(group.label || ""), collapsed: !!group.collapsed, keep: !!group.keep, nodes: nodes(group.entries) }))
      .filter(group => group.nodes.length || group.keep);
    if (top.length || groups.length) sections.push({ id: String(section.id), label: String(section.label || ""), collapsed: !!section.collapsed, nodes: top, groups });
  }
  return { sections, placed };
}

/** The index's rows by id, and by the session id a row's id ends with (#s387): the monitor names a
 *  Codex row by its rollout (`rollout-<time>-<uuid>`), while Herdr's hooks may report the bare
 *  session id. A row's own id always wins over another's alias. */
export function rowLookup(rows = new Map()) {
  const lookup = new Map(rows);
  for (const [id, row] of rows) {
    const tail = /-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i.exec(id);
    if (tail && !lookup.has(tail[1])) lookup.set(tail[1], row);
  }
  return lookup;
}

/** The index's rows by the Herdr pane the monitor links each to (#s387): `<server>|<pane id>`, as
 *  a Herdr layout entry names its pane; the default server is `default`. */
export function paneLookup(rows = new Map()) {
  const panes = new Map();
  for (const row of rows.values()) {
    if (row.term === "herdr" && row.target) panes.set(`${row.herdrSession || "default"}|${row.target}`, row);
  }
  return panes;
}

/** The choices of an arranged section's own menu (#s387), as the Claude app's
 *  section menu offers them: "app" keeps what the app gave. */
export const SECTION_GROUPS = ["app", "none", "project", "date", "state"];
export const SECTION_SORTS = ["app", "updated", "created", "name"];
export const DEFAULT_SECTION_VIEW = Object.freeze({ group: "app", sort: "app", descending: true });

/** A remembered view, with anything unknown put back to the default. */
export function sectionView(saved) {
  return {
    group: SECTION_GROUPS.includes(saved?.group) ? saved.group : "app",
    sort: SECTION_SORTS.includes(saved?.sort) ? saved.sort : "app",
    descending: saved?.descending !== false,
  };
}

/** Whether a view leaves the section as the app arranged it. */
export const isDefaultView = view => { const v = sectionView(view); return v.group === "app" && v.sort === "app"; };

/** The date heads the Date grouping draws, newest first, by the reader's local day. */
export const DATE_GROUPS = ["Today", "Yesterday", "Previous 7 days", "Previous 30 days", "Older", "No date"];
export function dateGroup(ts, now = Date.now() / 1000) {
  if (!ts) return "No date";
  const day = new Date(now * 1000); day.setHours(0, 0, 0, 0);
  const start = day.getTime() / 1000;
  if (ts >= start) return "Today";
  if (ts >= start - 86400) return "Yesterday";
  if (ts >= start - 7 * 86400) return "Previous 7 days";
  if (ts >= start - 30 * 86400) return "Previous 30 days";
  return "Older";
}
const STATE_GROUPS = [["active", "Active"], ["blocked", "Blocked"], ["idle", "Idle"], ["", "Not in this monitor's list"]];

/**
 * An arranged section in the reader's own order and grouping (#s387): the app
 * decides which sessions the section holds and under which sub-heads; the reader decides, per
 * section, how they are sorted (as the app gave them, last updated, date created or name, either
 * way) and grouped (the app's sub-heads, none, project, date or state). A session's nested ones
 * stay under it. `facts` reads a node: `updated`, `created` (seconds), `name`, `project` (a label)
 * and `state` ("active" | "blocked" | "idle", "" for a session the monitor has no row for), and
 * `now` (seconds). Ties keep the app's order, so a sort is stable across refreshes.
 */
export function viewSection(section, view, facts) {
  const v = sectionView(view);
  const order = nodes => {
    if (v.sort === "app") return nodes;
    const key = v.sort === "name" ? node => String(facts.name(node) || "").toLowerCase() : v.sort === "created" ? node => facts.created(node) || 0 : node => facts.updated(node) || 0;
    const sign = v.descending ? -1 : 1;
    return nodes.map((node, at) => [node, at, key(node)])
      .sort(([, i, x], [, j, y]) => (x < y ? -sign : x > y ? sign : 0) || i - j)
      .map(([node]) => node);
  };
  if (v.group === "app") return { ...section, nodes: order(section.nodes), groups: section.groups.map(group => ({ ...group, nodes: order(group.nodes) })) };
  const all = [...section.nodes, ...section.groups.flatMap(group => group.nodes)];
  if (v.group === "none") return { ...section, nodes: order(all), groups: [] };
  const labelOf = v.group === "project" ? node => facts.project(node) || "No project"
    : v.group === "date" ? node => dateGroup(facts.updated(node), facts.now)
    : node => (STATE_GROUPS.find(([key]) => key === (facts.state(node) || "")) || STATE_GROUPS[3])[1];
  const buckets = new Map();
  for (const node of all) { const label = labelOf(node); if (!buckets.has(label)) buckets.set(label, []); buckets.get(label).push(node); }
  const rank = v.group === "date" ? label => DATE_GROUPS.indexOf(label) : v.group === "state" ? label => STATE_GROUPS.findIndex(([, l]) => l === label) : null;
  const labels = [...buckets.keys()].sort((a, b) => rank ? rank(a) - rank(b) : a.localeCompare(b));
  return { ...section, nodes: [], groups: labels.map(label => ({ id: `${v.group}:${label}`, label, collapsed: false, keep: false, nodes: order(buckets.get(label)) })) };
}
