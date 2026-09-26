// The filter's one structural rule, for both pages (#118, design/rendering-parity-audit.md §5).
// A filter asks a question of a record — "is this a Bash call?", "is this one of the tools the
// reader ticked?" — and the answer is never about that record alone: a call sits inside an
// activity or a process record, so a parent whose CHILD matches is part of the answer too, and
// every fold on the way down has to open or the hit the reader was promised is invisible. Both
// pages walked that chain in their own words. The walk is here now; what a match MEANS (which
// records match, what opening one does) stays with each page.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

/** Walk a record and its nested blocks depth-first. `matches(record)` says whether the record
 *  itself is what the filter asked for; `onHit(record, direct)` is called for every record on a
 *  matching chain — the matching record and every ancestor that contains one — with `direct`
 *  telling the two apart. Returns whether this record or anything under it matched. Children
 *  are walked BEFORE the callback fires, so a page may open a parent knowing its subtree. */
function chainWalk(record, matches, onHit) {
  const own = !!matches(record);
  let inner = false;
  for (const part of record.body || []) {
    if (part.p !== "blocks") continue;
    for (const item of part.items || []) if (chainWalk(item, matches, onHit)) inner = true;
  }
  const hit = own || inner;
  if (hit && onHit) onHit(record, own);
  return hit;
}

/** The tool menu's SHAPE, for both pages (#293, absorbing #120): a session's tool calls as rows,
 *  with MCP calls collapsed into one expandable family instead of one row per `mcp__server__tool`
 *  — which is what a session with dozens of them needs, and what the classic page has always done.
 *
 *  `counts` is `{tool: n}`. `open` is the set of expanded twisty keys (`"mcp"`, `"mcp/<server>"`).
 *  `order` is `"label"` (the classic page's, alphabetical) or `"count"` (the app shell's, most-used
 *  first, ties alphabetical, the MCP family last).
 *
 *  Returns GROUPS, each `{label, count, rows}`: a group is one top-level entry and the rows its
 *  expansion currently shows, in order. A page sorts groups among its own non-tool rows by `label`
 *  (the classic page's kinds: Agent, Thinking, Activity…) and renders each group's rows as it
 *  draws rows — so the shape is decided once and the DOM stays each page's own.
 *
 *  Each row: `{label, count, depth, select, twisty, tint}` — `select` is what the row FILTERS by
 *  (`{tool}` exactly, or `{toolPre}` for a family, null for nothing), `twisty` the key it expands,
 *  `tint` the bullet class the classic page gives server and leaf rows. A server with ONE tool is
 *  compressed into a single `server/tool` row that filters that tool exactly — a twisty onto one
 *  child is a click that tells the reader nothing. */
function toolTree(counts, open, order = "label") {
  const plain = [];
  const mcp = { total: 0, servers: new Map() };
  for (const name of Object.keys(counts || {})) {
    const n = counts[name] || 0;
    const m = /^mcp__(.+?)__(.+)$/.exec(name);
    if (!m) {
      plain.push({ label: name, count: n, tool: name });
      continue;
    }
    mcp.total += n;
    const server = mcp.servers.get(m[1]) || { count: 0, tools: new Map() };
    server.count += n;
    server.tools.set(m[2], (server.tools.get(m[2]) || 0) + n);
    mcp.servers.set(m[1], server);
  }
  const has = key => !!(open && (open.has ? open.has(key) : open[key]));
  const groups = plain.map(t => ({
    label: t.label,
    count: t.count,
    rows: [{ label: t.label, count: t.count, depth: 0, select: { tool: t.tool }, twisty: null, tint: null }],
  }));
  if (mcp.total > 0) {
    const rows = [{ label: "MCP", count: mcp.total, depth: 0, select: { toolPre: "mcp__" }, twisty: "mcp", tint: null }];
    if (has("mcp")) {
      for (const server of [...mcp.servers.keys()].sort()) {
        const { count, tools } = mcp.servers.get(server);
        const names = [...tools.keys()].sort();
        if (names.length === 1) {
          rows.push({
            label: `${server}/${names[0]}`,
            count,
            depth: 1,
            select: { tool: `mcp__${server}__${names[0]}` },
            twisty: null,
            tint: "srv",
          });
          continue;
        }
        const key = `mcp/${server}`;
        rows.push({ label: server, count, depth: 1, select: { toolPre: `mcp__${server}__` }, twisty: key, tint: "srv" });
        if (!has(key)) continue;
        for (const tool of names) {
          rows.push({
            label: tool,
            count: tools.get(tool),
            depth: 2,
            select: { tool: `mcp__${server}__${tool}` },
            twisty: null,
            tint: "leaf",
          });
        }
      }
    }
    groups.push({ label: "MCP", count: mcp.total, rows, mcp: true });
  }
  if (order === "count") {
    // Most-used first, ties alphabetical, and the family last however big it is: a reader looking
    // for one MCP tool opens the family; a reader scanning for their own busiest tool should not
    // have to read past it.
    groups.sort((a, b) => (!!a.mcp - !!b.mcp) || b.count - a.count || a.label.localeCompare(b.label));
  } else {
    groups.sort((a, b) => a.label.localeCompare(b.label));
  }
  return groups;
}

export { chainWalk, toolTree };
