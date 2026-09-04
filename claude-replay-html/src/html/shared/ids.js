// shared: ids — how a session id is shown short, the same way on every page.
//
// A Claude/Codex session id is a UUID, and a Codex rollout name ends in one behind a date —
// `rollout-2026-08-09T12-00-00-<uuid>` — so the eight hex digits that start the UUID are the
// part a reader recognizes across a rail row, a header chip and a title. Anything else keeps
// its first eight characters once it is long enough to need it. The full value stays in the
// tooltip and on the clipboard; only what is SHOWN is short.

/** The recognizable short form of a session id. */
function snipId(s) {
  const id = String(s || "");
  const uuid = id.match(/(?:^|-)([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
  return uuid ? uuid[1] : (id.length > 12 ? id.slice(0, 8) : id);
}

export { snipId };
