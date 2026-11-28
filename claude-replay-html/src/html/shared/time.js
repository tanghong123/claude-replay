// Times as a reader wants them, one rule for both pages (#112, design/rendering-parity-audit.md
// row 3.19): a turn from today shows a bare clock time; an older turn carries its date, and the
// year only when it differs from now — a bare clock time on a week-old turn identifies nothing.
// "Now" is the browser's on purpose (a dump renders live; its bytes never bake a render date),
// and injectable so the rule is testable.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

/** The locale the transcript is written in. `[]` — the runtime default — made the format follow
 *  the OPERATING SYSTEM, so a machine set to zh-Hans rendered "Sep 3" as "9月3日" in a page whose
 *  every other string is English (the owner hit exactly that). The doc comment above has always
 *  promised "Mon D h:mm"; this makes the code keep that promise, and makes the module's own
 *  behaviour independent of whichever machine runs its tests. */
const LOCALE = "en-US";

/** `ts` in seconds since the epoch → "h:mm" today, "Mon D h:mm" this year, "Mon D, YYYY h:mm" otherwise. */
function fmtTime(ts, now = new Date()) {
  try {
    const d = new Date(ts * 1000);
    const t = d.toLocaleTimeString(LOCALE, { hour: "numeric", minute: "2-digit" });
    if (d.toDateString() === now.toDateString()) return t;
    const opts = { month: "short", day: "numeric" };
    if (d.getFullYear() !== now.getFullYear()) opts.year = "numeric";
    return d.toLocaleDateString(LOCALE, opts) + " " + t;
  } catch (_) { return ""; }
}

/** A duration in seconds → "Xh Ym", "Ym" or "Ns"; nothing for none.
 *  Under a minute it reads in SECONDS: 16% of turns are shorter than that (#257), and rounding
 *  a 19-second turn to "0m" says nothing while claiming to. */
function fmtDur(s) {
  if (!s || s < 0) return "";
  if (s < 60) return Math.round(s) + "s";
  const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60);
  return h ? h + "h " + m + "m" : m + "m";
}

/** A measured output rate, or no reading for invalid/empty measurements. */
function answerRate(a) {
  if (!a || !Number.isFinite(a.tok) || !Number.isFinite(a.ms) || a.tok <= 0 || a.ms <= 0) return "";
  const rounded = Math.round(a.tok * 1000 / a.ms);
  return rounded ? `${rounded} tok/s` : "<1 tok/s";
}

/** The one line under a turn and its explanation. */
function turnReading(secs, answer) {
  const worked = typeof secs === "number" && secs > 0 ? `worked ${fmtDur(secs)}` : "";
  const speed = answerRate(answer);
  const text = [worked, speed].filter(Boolean).join(" · ");
  if (!text) return null;
  const n = answer?.n || 0;
  const duration = speed && answer.ms < 50 ? "<0.1" : speed ? (answer.ms / 1000).toFixed(1) : "";
  const title = speed
    ? `${Number(answer.tok).toLocaleString("en-US")} output tokens in ${duration} s of generation (${n} ${n === 1 ? "answer" : "answers"}, ${answer.model}; includes first-token wait, excludes tool time between answers)`
    : "How long the agent worked on this turn";
  return { text, title };
}

export { fmtTime, fmtDur, answerRate, turnReading };
