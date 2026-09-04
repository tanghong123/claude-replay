// Times as a reader wants them, one rule for both pages (#112, design/rendering-parity-audit.md
// row 3.19): a turn from today shows a bare clock time; an older turn carries its date, and the
// year only when it differs from now — a bare clock time on a week-old turn identifies nothing.
// "Now" is the browser's on purpose (a dump renders live; its bytes never bake a render date),
// and injectable so the rule is testable.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

/** `ts` in seconds since the epoch → "h:mm" today, "Mon D h:mm" this year, "Mon D, YYYY h:mm" otherwise. */
function fmtTime(ts, now = new Date()) {
  try {
    const d = new Date(ts * 1000);
    const t = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    if (d.toDateString() === now.toDateString()) return t;
    const opts = { month: "short", day: "numeric" };
    if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
    return d.toLocaleDateString([], opts) + " " + t;
  } catch (_) { return ""; }
}

/** A duration in seconds → "Xh Ym" or "Ym"; nothing for none. */
function fmtDur(s) {
  if (!s || s < 0) return "";
  const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60);
  return h ? h + "h " + m + "m" : m + "m";
}

export { fmtTime, fmtDur };
