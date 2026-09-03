// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…) and the
// classic session page (inlined into the self-contained export by the html crate — see
// html_export/shared.rs). Conventions this file must keep, because the inliner relies on
// them: no imports, and exactly one trailing `export { … };` line.
// Grouping and visibility of the session list — pure functions over the `/api/sessions`
// shape, with no DOM and no imports, so the contract test runs them under node.
//
// Hiding is the SERVER's state (`/api/ignore`, keys `s:<sid>` / `p:<cwd>` / `a:<label>`),
// and every row and group arrives with its own `ignoreKey` and a `hidden` flag that already
// folds a group's hiding into its rows. This module never re-derives a key — it passes the
// server's through — and it never drops a hidden row from the model: hidden is a VIEW
// filter, toggled by "Hidden (n)", exactly as the classic rail treats it (#113).

/** agents → projects → sessions, from the `/api/sessions` groups. */
function groupSessions(groups = []) {
  const agents = new Map();
  for (const group of groups) for (const row of group.rows || []) {
    const agentId = row.agent || "other";
    if (!agents.has(agentId)) agents.set(agentId, { id: agentId, projects: new Map() });
    const agent = agents.get(agentId);
    const desktop = group.kind === "agent";
    const projectId = desktop ? `${agentId}:desktop` : group.ignoreKey || group.label;
    if (!agent.projects.has(projectId)) agent.projects.set(projectId, {
      id: projectId,
      name: desktop ? "Desktop sessions" : group.label,
      path: group.secondary || "",
      kind: group.kind || "project",
      ignoreKey: group.ignoreKey || "",
      hidden: !!group.hidden,
      sessions: []
    });
    agent.projects.get(projectId).sessions.push(row);
  }
  return [...agents.values()].map(agent => ({ ...agent, projects: [...agent.projects.values()] }));
}

/**
 * The tree the sidebar draws: agents whose projects have at least one row to show, each
 * project carrying only those rows. `showHidden` admits hidden projects and rows;
 * `attention` keeps only rows `needs(row)` says need a person. A hidden project stays
 * hidden as a whole unless `showHidden`, even when a row inside it is not individually
 * hidden — the server marks such rows hidden too, so both filters agree.
 */
function visibleTree(agents, { showHidden = false, attention = false, needs = () => true } = {}) {
  const keep = row => rowVisible(row, { showHidden, attention, needs });
  const out = [];
  for (const agent of agents) {
    const projects = [];
    for (const project of agent.projects) {
      if (!groupVisible(project, { showHidden })) continue;
      const rows = project.sessions.filter(keep);
      if (rows.length) projects.push({ ...project, rows });
    }
    if (projects.length) out.push({ ...agent, projects });
  }
  return out;
}

/**
 * Whether a row is shown: a hidden row only under `showHidden`; with `attention`, only the
 * rows `needs` says need a person. The classic rail's `okHidden` and the app shell's tree
 * filter are this one predicate (#113).
 */
function rowVisible(row, { showHidden = false, attention = false, needs = () => true } = {}) {
  return (showHidden || !row.hidden) && (!attention || needs(row));
}

/** Whether a group — a project, or a desktop agent — is shown at all: hidden only under `showHidden`. */
function groupVisible(group, { showHidden = false } = {}) {
  return showHidden || !group.hidden;
}

/** What the row action does for a session or a project, and how it is labelled. */
function hideAction(target, kind = "session") {
  const noun = kind === "agent" ? "agent" : kind === "project" ? "project" : "session";
  return target.hidden
    ? { op: "remove", key: target.ignoreKey, title: `Restore this ${noun}`, icon: "back" }
    : { op: "add", key: target.ignoreKey, title: `Hide this ${noun}`, icon: "x" };
}

/** The `/api/ignore` query for an action, verbatim key — the server owns the grammar. */
const ignoreQuery = ({ op, key }) => `/api/ignore?${op}=${encodeURIComponent(key)}`;

/**
 * Fork families (#142): rows sharing a `family` root are one conversation forked — they
 * overlap heavily, so the list shows one representative with a fork count, and the other
 * members on demand. The representative is the root (the member that is not a fork); when
 * the root is not among the rows (filtered, hidden, or its transcript gone) the most recently
 * active member speaks for the family. A family is growing if any member is, and its
 * activity is its newest member's. Order follows first appearance, so it is stable across
 * polls. (Sub-agent CHILDREN are not rows at all — they are reached through a session's
 * `children` and come back through its `ancestors`; see the parent control.)
 */
/**
 * What counts as ONE row (#153). For most agents that is the fork family (#142): the server's
 * `family` root, else the row itself. For QoderWork it is the TITLE, " (Fork)" stripped —
 * measured on one store, 37 sessions carried 19 distinct titles, and six titles spanned
 * SEPARATE fork families (four sessions of one interview-prep task in four families), which
 * family grouping cannot merge because they are not forks of each other. A title is a label
 * rather than an identity, so two genuinely distinct chats sharing a name will cluster; they
 * are indistinguishable to the reader anyway, and the row expands. The classic rail's rule,
 * now the one rule.
 */
function familyKey(row) {
  if ((row.agent || "").toLowerCase().includes("qoder")) {
    const title = (row.name || "").replace(" (Fork)", "").trim();
    if (title) return `t:${title}`;
  }
  return row.family || row.id;
}

function families(rows = []) {
  const by = new Map();
  for (const row of rows) {
    const key = familyKey(row);
    if (!by.has(key)) by.set(key, { key, rep: null, members: [], growing: false, latest: 0 });
    const family = by.get(key);
    family.members.push(row);
    family.latest = Math.max(family.latest, row.activityTs || 0);
    if (row.state === "growing") family.growing = true;
    if (!row.isFork && !family.rep) family.rep = row;
  }
  for (const family of by.values()) {
    if (!family.rep) family.rep = [...family.members].sort((a, b) => (b.activityTs || 0) - (a.activityTs || 0))[0];
    family.forks = family.members.filter(row => row !== family.rep);
  }
  return [...by.values()];
}

export { groupSessions, visibleTree, rowVisible, groupVisible, hideAction, ignoreQuery, familyKey, families };
