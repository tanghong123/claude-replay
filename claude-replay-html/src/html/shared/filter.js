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

export { chainWalk };
