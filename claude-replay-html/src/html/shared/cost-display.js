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

// The agent CLIENT's own recorded cost (#240), phrased so it can never be mistaken for the
// session total. Claude Code writes `cost-state` per CLI PROCESS, so this figure is a WINDOW:
// it starts when some process started and stops when that process ended, while the transcript
// spans everything. Measured on two real sessions, the client's tally covered 08-27..09-03 of
// sessions that began in June and July and were still running on 09-18 — $27.44 against our
// $659 and $753.14 against our $8,123. Both numbers are right about different spans, which is
// exactly why the window has to travel with the figure.
//
// Where they OVERLAP the two agree — sliced to the client's own window our per-call sums come
// to 0.97x and 1.00x of its cache-read tokens — so this is a cross-check on our pricing, not a
// competitor to it. Returns null when the transcript records no tally.
function reportedCostDisplay(reported) {
  if (!reported || !reported.cost) return null;
  const day = ms => {
    if (ms == null) return null;
    const d = new Date(ms);
    return Number.isNaN(d.getTime())
      ? null
      : d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  };
  const from = day(reported.from), through = day(reported.through);
  // The client flags a model it could not price itself (`hasUnknownModelCost`), which makes
  // even its in-window figure a LOWER BOUND. Marked the same way our own partial estimate is,
  // so one reader learns one convention: `≥` means "at least this", `~` means "about this".
  const label = reported.unknown_model ? `≥${reported.cost}` : reported.cost;
  const priced = reported.unknown_model
    ? " Claude Code could not price one of the models involved, so its own figure is a floor."
    : "";
  // A complete window needs no qualifier: the figure simply is the session's.
  if (reported.complete) {
    return { label, note: null, complete: true,
             title: "Claude Code's own recorded cost for this session." + priced };
  }
  const span = from && through && from !== through ? `${from} – ${through}`
             : through || from || null;
  const note = span ? `counted ${span}` : "partial";
  const epochs = reported.epochs > 1 ? `, over ${reported.epochs} runs` : "";
  return {
    label,
    note,
    complete: false,
    title: `Claude Code counted ${reported.cost}${span ? ` between ${span}` : ""}${epochs}.`
      + " It records cost only while its own process is running, so this covers part of the"
      + " session — the estimate above covers all of it." + priced,
  };
}

export { costDisplay, reportedCostDisplay };
