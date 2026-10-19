/** Exact file identity bridges native thread IDs to Monitor's established rollout stream. */
export function monitorSessionId(thread) {
  return typeof thread?.path === "string" && thread.path.endsWith(".jsonl") ? thread.path.split("/").at(-1).slice(0, -6) : `native:${thread.id}`;
}

/** Infer display membership only for unassigned threads with one matching project. */
function displayProject(thread, projects) {
  if (thread.projectId) return projects.find(project => project.id === thread.projectId);
  if (typeof thread.cwd !== "string" || !thread.cwd.startsWith("/")) return undefined;
  const cwd = thread.cwd.replace(/\/+$/, "") || "/";
  const matches = projects.filter(project => (project.roots || []).some(root => {
    if (typeof root?.path !== "string" || !root.path.startsWith("/")) return false;
    const path = root.path.replace(/\/+$/, "") || "/";
    return cwd === path || cwd.startsWith(path === "/" ? "/" : `${path}/`);
  }));
  return matches.length === 1 ? matches[0] : undefined;
}

/** Native history owns visibility; original Monitor rows retain parser/runtime metadata. */
export function nativeSessionGroups(legacy, threads, projects) {
  const rows = new Map((legacy.groups || []).flatMap(group => (group.rows || []).map(row => [row.id, row])));
  const hiddenProjects = new Map((legacy.groups || []).map(group => [group.ignoreKey, !!group.hidden]));
  const resolved = threads.map(thread => ({ thread, project: displayProject(thread, projects) }));
  const projectScopes = new Map();
  for (const { thread, project } of resolved) {
    if (!project) continue;
    if (!projectScopes.has(project.id)) projectScopes.set(project.id, { directories: new Set(), assigned: new Set(), legacy: new Set() });
    const scope = projectScopes.get(project.id);
    scope.directories.add(thread.cwd);
    (thread.projectId ? scope.assigned : scope.legacy).add(thread.cwd);
  }
  // A legacy/native pair may join one directory without taking another directory's Hide/Restore target.
  const splitProjects = new Set([...projectScopes].filter(([, scope]) => scope.assigned.size > 1 && [...scope.legacy].some(cwd => scope.assigned.has(cwd))).map(([id]) => id));
  const groups = new Map();
  for (const { thread, project: candidate } of resolved) {
    const id = monitorSessionId(thread), previous = rows.get(id);
    const scope = projectScopes.get(candidate?.id);
    const project = thread.projectId || scope?.assigned.has(thread.cwd) || scope?.directories.size === 1 ? candidate : undefined;
    const projectDirectory = project && splitProjects.has(project.id) ? thread.cwd : null;
    const key = projectDirectory !== null ? JSON.stringify([project.id, projectDirectory]) : project?.id || thread.cwd;
    if (!groups.has(key)) groups.set(key, { kind: "project", projectId: project?.id || null, ...(projectDirectory !== null ? { projectDirectory, projectPrimaryDirectory: project.roots?.[0]?.path } : {}), label: project?.name || thread.cwd?.split("/").at(-1) || "Codex", secondary: thread.cwd, ignoreKey: `p:${thread.cwd}`, hidden: hiddenProjects.get(`p:${thread.cwd}`) || false, rows: [] });
    const status = thread.status?.type;
    const flags = thread.status?.activeFlags || [];
    const running = status === "active" || status === "running";
    const waitingReason = running && (flags.includes("waitingOnApproval") ? "permission" : flags.includes("waitingOnUserInput") ? "question" : null);
    const terminalReasons = ["ended-question", "error", "failed", "stalled", "exited-mid-work"];
    const currentTerminalReason = previous?.agentState === "idle" && previous?.stateConfidence === "observed"
      && previous.activityTs >= (thread.updatedAt || 0) && terminalReasons.includes(previous.stateReason) ? previous.stateReason : "done";
    // Native liveness replaces the correlated verdict, including the reason consumed by all Monitor buckets.
    const verdict = running ? { state: "growing", agentState: waitingReason ? "wait" : "busy", stateReason: waitingReason || "thinking", stateConfidence: "observed" }
      : status === "idle" ? { state: "idle", agentState: "idle", stateReason: currentTerminalReason, stateConfidence: "observed" }
      : status === "systemError" ? { state: "idle", agentState: "idle", stateReason: "failed", stateConfidence: "observed" }
      : {};
    if (verdict.stateReason) {
      verdict.stateDetail = verdict.stateReason === currentTerminalReason && currentTerminalReason !== "done" ? previous.stateDetail : `Native Codex status: ${waitingReason || status}`;
      verdict.stateSince = thread.updatedAt || null;
    }
    const row = { ...previous, id, nativeThreadId: thread.id, cwd: thread.cwd, projectId: thread.projectId, name: thread.name || thread.preview || "New session", agent: "codex", activityTs: thread.updatedAt || 0, startTs: thread.createdAt || 0,
      state: previous?.state || "idle", agentState: previous?.agentState || "idle", ...verdict, nativeStatus: thread.status,
      ignoreKey: previous?.ignoreKey || `s:${id}`, family: previous?.family || id };
    groups.get(key).rows.push(row);
  }
  // Native history only replaces Codex membership; other agents keep every original row and group field.
  const nativeIds = new Set(threads.map(monitorSessionId));
  const ordinaryGroups = (legacy.groups || []).map(group => ({ ...group,
    rows: (group.rows || []).filter(row => !nativeIds.has(row.id) && String(row.agent || "").toLowerCase() !== "codex")
  })).filter(group => group.rows.length);
  return { ...legacy, groups: [...ordinaryGroups, ...groups.values()] };
}

/** Only advertised model tiers can be chosen; null is explicit normal speed. */
export function tierOptions(model) {
  return [{ id: null, name: "Standard", description: "Standard speed" }, ...(model?.tiers || [])];
}

/** Keep native projects and their independent directory controls distinct. */
export function nativeSessionTree(groups, build) {
  // The primary directory keeps its established view identity when another directory arrives or leaves.
  const prepared = groups.map(group => ({ ...group, ignoreKey: group.projectId ? typeof group.projectDirectory === "string" && group.projectDirectory !== group.projectPrimaryDirectory ? `native-project-directory:${JSON.stringify([group.projectId, group.projectDirectory])}` : `native-project:${group.projectId}` : group.ignoreKey }));
  const originals = new Map(prepared.map((group, index) => [group.ignoreKey, groups[index]]));
  return build(prepared).map(agent => ({ ...agent, projects: agent.projects.map(project => ({ ...project, nativeProjectId: originals.get(project.ignoreKey)?.projectId || "", ignoreKey: originals.get(project.ignoreKey)?.ignoreKey || project.ignoreKey })) }));
}

/** Saved projects stay available for creation even before they contain a visible session. */
export function nativeProjectTree(visible, all, projects) {
  // While the native inventory loads, directory groups already visible in the reader
  // can own the create action when their exact root identifies one saved project.
  const annotate = group => {
    if (group.nativeProjectId) return group;
    const matches = projects.filter(project => project.roots?.some(root => root.path === group.path));
    return matches.length === 1 ? { ...group, nativeProjectId: matches[0].id } : group;
  };
  const result = visible.map(agent => ({ ...agent, projects: agent.id === "codex" ? agent.projects.map(annotate) : [...agent.projects] }));
  let codex = result.find(agent => agent.id === "codex");
  if (!codex) { codex = { id: "codex", name: "Codex", projects: [] }; result.push(codex); }
  const known = (all.find(agent => agent.id === "codex")?.projects || []).map(annotate);
  for (const project of projects) {
    if (codex.projects.some(group => group.nativeProjectId === project.id)) continue;
    // A hidden project remains hidden. Filtering its sessions does not remove its create action.
    const existing = known.filter(group => group.nativeProjectId === project.id);
    if (existing.length) {
      codex.projects.push(...existing.filter(group => !group.hidden).map(group => ({ ...group, rows: [] })));
    } else codex.projects.push({ id: `native-project:${project.id}`, nativeProjectId: project.id,
      name: project.name, path: project.roots?.[0]?.path || "", kind: "project", sessions: [], rows: [] });
  }
  return result;
}
