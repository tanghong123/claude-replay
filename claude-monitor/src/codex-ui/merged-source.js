// The merged source (#s541, design/monitor-hub.md D6; Hong's choice on #s539: "build the merge into
// the kit (each part its own server, a machine label and stale heads on rows)"). ONE server, answered
// in the page, over the session lists of several machines, each read through that machine's own
// backend handle. A host that lists the sessions of more than one machine (the native app, over
// every paired Mac) hands it to the session list as its `backend`; the list draws what it answers as
// it draws one monitor's, and reads the row's `machine` for its label.
//
// It answers the routes the list reads, and no other:
// - `/api/sessions`: every machine's groups, each row and group naming its machine in `machine`, and
//   every id and key the list matches on written `<machine>/<id>`, so the same path on two machines
//   is two groups and nothing nests across machines; and `machines`, how each machine answered: its
//   label, `live`, `stale` (its last answer, kept), `waiting` (no answer yet) or `failed` (none, and
//   why), and since when its rows are from.
// - `/api/ignore`: a hide or a restore, sent to the row's machine with the machine taken off the key.
// - `/api/layout`: no other app's arrangement. Each machine has its own apps, and N apps' orders make
//   no one list; they come with the arrangement by machine (D7, phase 2). Each source is answered as
//   one that cannot be read, so the list offers it with the reason and lets nobody choose it.
// Nothing else: the view reads one machine's handle, never this one (D8).
//
// It keeps nothing to stop. Each ask of the list asks every machine that has no ask still out, waits
// at most `timeout` for the machines that were on time before, and answers with each machine's last
// good answer, so one slow machine never holds the list (R6): a machine already late is not waited
// for again, and its answer is kept for the next ask when it comes. An ask still out after eight
// times `timeout` is given up (its fetch aborted), so a machine gone quiet is asked afresh.

/** The routes the merged source answers in the page, and asks each machine's handle for. */
const SESSIONS = "/api/sessions", LAYOUT = "/api/layout", IGNORE = "/api/ignore";

/** The other apps whose arrangement one monitor's list can follow (session-list.js `ARRANGE_SOURCES`). */
const SOURCES = Object.freeze(["claude-app", "codex-app", "herdr"]);

/** A row's fields that name a session, written `<machine>/<id>`: the ones the list matches on. */
const ROW_IDS = Object.freeze(["id", "family", "sideOf", "startedBy", "continuedIn", "ignoreKey"]);

const answer = (body, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const tagged = (machine, value) => `${machine}/${value}`;

/** A merged id or key as its machine and the machine's own (`"mac-mini/s:abc"` → `{ machine:
 *  "mac-mini", id: "s:abc" }`), split at the first "/", which no machine's key holds; null for one
 *  with no machine. */
export function splitMachineId(id) {
  const text = typeof id === "string" ? id : "";
  const at = text.indexOf("/");
  return at > 0 ? { machine: text.slice(0, at), id: text.slice(at + 1) } : null;
}

/** One machine's group as the merged list holds it. */
function mergeGroup(machine, group) {
  const rows = (group.rows || []).map(row => {
    const out = { ...row, machine };
    for (const field of ROW_IDS) if (typeof row[field] === "string" && row[field]) out[field] = tagged(machine, row[field]);
    return out;
  });
  const out = { ...group, machine, rows };
  if (typeof group.ignoreKey === "string" && group.ignoreKey) out.ignoreKey = tagged(machine, group.ignoreKey);
  return out;
}

/** The merged source over `machines`, each `{ machine, label, handle }`: a key of the host's with no
 *  "/" in it (the native app's machine id), the name a reader knows it by, and its backend handle.
 *  `timeout` is how long an ask waits for the machines (ms); `now` the clock, for a test.
 *
 *  It returns a backend handle the list takes as its `backend`, with three members of its own:
 *  `machines()`, each machine's state as the last answer said it; `locate(id)`, a merged row's
 *  machine (`{ machine, label, handle, id, row }`, `id` and `row` as that machine wrote them, or
 *  null); and `forView(list, machine)`, the session view's row options for a view mounted on that
 *  machine's handle (`selectedId`, `sessionRow`, `sessionWorkers`, in the machine's own ids), with
 *  `listId(id)`, the list's id for one of them. Its URLs are refused: nothing is read from it but
 *  the list's routes. */
export function mergedBackend(machines, { timeout = 4000, now = () => Date.now() } = {}) {
  if (!Array.isArray(machines) || !machines.length) throw new Error("mergedBackend takes the machines it merges, as [{ machine, label, handle }]");
  const parts = new Map();
  for (const entry of machines) {
    const machine = entry?.machine;
    if (typeof machine !== "string" || !machine || machine.includes("/")) throw new Error(`mergedBackend names each machine by a key with no "/" in it, not ${JSON.stringify(machine ?? null)}`);
    if (parts.has(machine)) throw new Error(`mergedBackend has two machines named ${machine}`);
    if (typeof entry.handle?.fetch !== "function") throw new Error(`mergedBackend reads ${machine} through a backend handle (backendHandle()), and was given none`);
    parts.set(machine, { machine, label: String(entry.label || machine), handle: entry.handle, data: null, rows: new Map(), at: null, error: null, overdue: false, out: null });
  }
  const seconds = ms => ms >= 1000 ? `${Number((ms / 1000).toFixed(1))} s` : `${ms} ms`;
  const waited = seconds(timeout), hold = timeout * 8;

  /** A machine's answer kept, with its rows by id as the view reads them: each with its group, whose
   *  label and directory the view's head and Info name, as a monitor's own list gives its rows. */
  const keep = (part, data) => {
    part.data = data && typeof data === "object" ? data : { groups: [] };
    part.rows = new Map();
    for (const group of part.data.groups || []) for (const row of group.rows || []) if (typeof row?.id === "string") part.rows.set(row.id, { ...row, _group: group });
  };

  // One ask of a machine at a time: a machine still answering the last ask is not asked again. One
  // whose answer an ask stopped waiting for is overdue until it answers, or until it is given up. Its
  // last trouble stands until the new ask settles, so a machine read while it is being asked again
  // reads as it last did; while it is overdue, that is what its state says.
  const ask = part => {
    if (part.out) return part.out;
    const stop = typeof AbortController === "function" ? new AbortController() : null;
    let gaveUp = false;
    const timer = setTimeout(() => { gaveUp = true; stop?.abort(); }, hold);
    const out = Promise.resolve()
      .then(() => part.handle.fetch(SESSIONS, stop ? { cache: "no-store", signal: stop.signal } : { cache: "no-store" }))
      .then(response => { if (!response.ok) throw new Error(`HTTP ${response.status}`); return response.json(); })
      .then(data => { keep(part, data); part.at = now(); part.error = null; },
        error => { part.error = gaveUp ? `no answer in ${seconds(hold)}` : String(error?.message || error || "no answer"); })
      .finally(() => { clearTimeout(timer); part.overdue = false; if (part.out === out) part.out = null; });
    part.out = out;
    return out;
  };
  const state = part => {
    const trouble = part.overdue ? `no answer in ${waited}` : part.error;
    return {
      machine: part.machine,
      label: part.label,
      state: part.data ? (trouble ? "stale" : "live") : trouble ? "failed" : "waiting",
      since: part.at == null ? null : new Date(part.at).toISOString(),
      error: trouble,
    };
  };

  const sessions = async () => {
    const asks = [...parts.values()].map(part => [part.overdue, ask(part)]).filter(([late]) => !late).map(([, out]) => out);
    let timer = 0;
    await Promise.race([Promise.all(asks), new Promise(resolve => { timer = setTimeout(resolve, timeout); })]);
    clearTimeout(timer);
    for (const part of parts.values()) if (part.out) part.overdue = true;
    const groups = [];
    let ignoredCount = 0;
    for (const part of parts.values()) {
      if (!part.data) continue;
      ignoredCount += Number(part.data.ignoredCount) || 0;
      for (const group of part.data.groups || []) groups.push(mergeGroup(part.machine, group));
    }
    return answer({ groups, ignoredCount, machines: [...parts.values()].map(state) });
  };

  const ignore = async query => {
    const op = query.has("add") ? "add" : query.has("remove") ? "remove" : null;
    const where = op ? splitMachineId(query.get(op)) : null;
    const part = where && parts.get(where.machine);
    if (!part || !where.id) return answer({ ok: false, error: "no machine of this list holds that key" }, 404);
    const response = await part.handle.fetch(`${IGNORE}?${op}=${encodeURIComponent(where.id)}`, { cache: "no-store" });
    if (!response.ok) return answer({ ok: false, error: `HTTP ${response.status}` }, response.status);
    return answer(await response.json());
  };

  const route = async path => {
    let url;
    try { url = new URL(String(path), "http://merged.invalid"); } catch (_) { return answer({ error: "not a server path" }, 404); }
    if (url.pathname === SESSIONS) return sessions();
    if (url.pathname === IGNORE) return ignore(url.searchParams);
    if (url.pathname === LAYOUT) return url.searchParams.has("source") ? answer({ error: "no other app's arrangement over several machines" }, 404) : answer({ sources: SOURCES.map(source => ({ source, available: false, reason: "Not over several machines" })) });
    return answer({ error: `the merged source answers the session list's routes alone, not ${url.pathname}` }, 404);
  };

  const locate = id => {
    const where = splitMachineId(id);
    const part = where && parts.get(where.machine);
    return part ? { machine: part.machine, label: part.label, handle: part.handle, id: where.id, row: part.rows.get(where.id) || null } : null;
  };
  const refused = name => () => { throw new Error(`the merged source answers the session list's routes alone; it has no ${name}`); };

  return Object.freeze({
    base: "",
    local: false,
    paired: false,
    version: null,
    mdrevVersion: () => "",
    mdrevMoved: () => {},
    url: refused("URL"),
    fetch: (path, _init) => route(path),
    mdrevUrl: refused("mdrev"),
    mdrevFetch: refused("mdrev"),
    machines: () => [...parts.values()].map(state),
    locate,
    forView: (list, machine) => {
      if (!parts.has(machine)) throw new Error(`the merged source has no machine named ${machine}`);
      const listId = id => tagged(machine, id);
      return {
        listId,
        selectedId: () => { const where = splitMachineId(list.selected()); return where && where.machine === machine ? where.id : ""; },
        sessionRow: id => locate(listId(id))?.row || null,
        sessionWorkers: id => list.workers(listId(id)).map(row => locate(row.id)?.row).filter(Boolean),
      };
    },
  });
}
