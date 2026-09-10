// One cost state for every monitor shell. `costPartial` is meaningful even without `cost`:
// that combination means token usage exists but no model in it has an evidenced price.
function costDisplay(row, compact = false) {
  const raw = row && row.cost != null ? Number(row.cost) : null;
  const known = raw != null && Number.isFinite(raw) ? raw : null;
  const partial = !!(row && row.costPartial);
  if (known == null) {
    return partial
      ? { kind: "unpriced", known: null, label: "unpriced" }
      : { kind: "none", known: null, label: null };
  }
  const prefix = partial ? "≥" : "~";
  let amount;
  if (!compact) amount = `$${known.toFixed(2)}`;
  else if (known >= 1000) amount = `$${(known / 1000).toFixed(1)}k`;
  else if (known >= 100) amount = `$${known.toFixed(0)}`;
  else amount = `$${known.toFixed(2)}`;
  return { kind: partial ? "partial" : "priced", known, label: prefix + amount };
}

export { costDisplay };
