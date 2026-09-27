// The ⌘K box's own query (design/global-search.md): a jump-to over agents, projects and sessions.
// It knows exactly two qualifiers, `agent:` and `project:`, and everything else is text matched
// against names. It shares NOTHING with the in-session search grammar (shared/search.js) — the
// owner, 2026-09-28: "we should decouple mac-K from the shared grammar entirely" — so a change to
// the session box's filters can never change what this box finds.

/** `raw` → `{agents, projects, text}`: the values of every `agent:`/`project:` token (lowercased,
 *  deduplicated, in the order typed) and the rest of the box, lowercased and trimmed, for matching
 *  names. A key with no value (`project:` mid-type) is text, so a reader mid-word is not suddenly
 *  searching nothing; a token's own leading space goes with it. */
function parseJumpQuery(raw) {
  const out = { agents: [], projects: [] };
  const rest = String(raw ?? "").replace(/(^|\s)(agent|project):(\S+)/gi, (whole, lead, key, value) => {
    const list = key.toLowerCase() === "agent" ? out.agents : out.projects;
    const v = value.toLowerCase();
    if (!list.includes(v)) list.push(v);
    return "";
  });
  out.text = rest.trim().toLowerCase();
  return out;
}

export { parseJumpQuery };
