// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
//
// The `/pull` + `/records` protocol's CLIENT HALF, once (#49, design/monitor-shell-duplication.md
// §1(e)). The server returns a self-describing pull reply with TWO zones — `committed`
// (permanent, append-only; sent as a POINTER `committed_ext: {offset, len}` into the on-disk
// record log, range-read through `/records`) and `provisional` (the open turn: truncate-from +
// append) — keyed by a 4-number cursor {epoch, committed, provisional_gen, provisional_index}.
// A client is CONTENT-BLIND: it applies committed appends and the provisional truncate/extend
// by position, never inspecting a block. `epoch` bumps on a server-side reset, and a fresh
// client's epoch 0 mismatches on purpose so its first pull resyncs. See cache::shared and
// serve.rs.
//
// What lives here: the cursor, the queries, the record parser, and the REDUCER — from a
// client's cursor, how many records it holds, and a reply, the plan its store applies and the
// next cursor. What stays with each page: its fetch loop (export.js: an interval, an inflight
// guard spanning both fetches, a self-heal resync on a torn apply; the app shell's store: a 1 s
// timer), its store and DOM (export.js `resetFrom`/`putBlock`; the store's array and
// `handlers.update`), and export.js's OFFLINE modes (`#session-data`, the static bundle), which
// never touch the reducer.

/** A cursor no server has seen — its epoch 0 resyncs on the first pull. */
const freshCursor = () => ({ epoch: 0, committed: 0, gen: 0, index: 0 });

/** The cursor as the wire spells it: `epoch.committed.gen.index`. */
const cursorText = c => `${c.epoch}.${c.committed}.${c.gen}.${c.index}`;

/** The `/pull` query, minus the route (the classic page fetches relative to its own path). */
const pullQuery = (session, cursor) => `session=${encodeURIComponent(session)}&cursor=${encodeURIComponent(cursorText(cursor))}`;

/** The `/records` range-read query for a reply's committed pointer, bound to its epoch — a
 *  pointer issued before a reset must not read a recreated log (the server answers 409). */
const recordsQuery = (session, ext, epoch) => `session=${encodeURIComponent(session)}&from=${ext.offset}&len=${ext.len}&epoch=${epoch}`;

/** The committed range's text as records, one per non-blank line. */
const parseRecords = text => String(text || "").split("\n").filter(l => l.trim()).map(l => JSON.parse(l));

/**
 * The two-zone reducer. `cursor` is the client's, `length` how many records it holds, `reply`
 * a pull reply whose `committed` has been materialized (an array; `[]` when the pointer was
 * empty). Returns:
 *  - `idle`: same epoch, BOTH ZONES EMPTY — the classic page's early-return rule (an idle
 *    reply's meta is null; each page keeps its own meta rule). Not "no steps": a shorter
 *    provisional zone with nothing to append is idle by this rule and still truncates, and
 *    `changedFrom` says so — the two answer different questions.
 *  - `resync`: the epoch moved — everything the client holds is dropped first.
 *  - `steps`: `{op: "truncate", to}` / `{op: "append", records}` in order — a commit (or the
 *    resync) truncates at `committed_from` (always `<=` the client's committed count) and
 *    appends the permanent blocks; the provisional zone truncates to the committed prefix +
 *    `provisional_from` and appends its suffix (a same-gen append keeps the prefix; a gen bump
 *    or a commit sends `provisional_from = 0` ⇒ replace).
 *  - `cursor`: the next cursor; `length`: how many records the client holds after the steps.
 *  - `changedFrom`: the first index whose content may differ — 0 on a resync, the earliest
 *    truncate point otherwise, `Infinity` when nothing changed.
 */
function reducePull(cursor, length, reply) {
  const committedZone = reply.committed || [];
  const provisional = reply.provisional || [];
  const resync = reply.epoch !== cursor.epoch;
  const idle = !resync && !committedZone.length && !provisional.length;
  const steps = [];
  let committed = resync ? 0 : cursor.committed;
  let held = resync ? 0 : length;
  let changedFrom = resync ? 0 : Infinity;
  if (resync) steps.push({ op: "truncate", to: 0 });
  if (committedZone.length) {
    changedFrom = Math.min(changedFrom, reply.committed_from);
    steps.push({ op: "truncate", to: reply.committed_from });
    steps.push({ op: "append", records: committedZone });
    committed = reply.committed_from + committedZone.length;
    held = committed;
  }
  const provisionalAt = committed + reply.provisional_from;
  if (held !== provisionalAt || provisional.length) {
    changedFrom = Math.min(changedFrom, provisionalAt);
    steps.push({ op: "truncate", to: provisionalAt });
    if (provisional.length) steps.push({ op: "append", records: provisional });
    held = provisionalAt + provisional.length;
  }
  const next = { epoch: reply.epoch, committed, gen: reply.provisional_gen, index: reply.provisional_from + provisional.length };
  return { idle, resync, steps, cursor: next, length: held, changedFrom };
}

export { freshCursor, cursorText, pullQuery, recordsQuery, parseRecords, reducePull };
