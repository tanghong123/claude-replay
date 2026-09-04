// Body parts → markup, with the ROW CAPS both pages honour (#108, design/rendering-parity-audit.md
// row 3.1). The server ships every `pre`/`num`/`diff` part with a `cap`: the first `cap` rows
// are what a reader sees unasked, the rest sit behind a "⋯ N more lines · to line M" control,
// and a SMALL expansion (within MAX_BUFFER_LINES) is remembered per record and ordinal so it
// survives rematerialization; a large one is ephemeral by design (a reset is welcome there).
// The classic page had all of this (export.js `capped`/`numberedRows`/`diffRows`, #67); the
// app shell rendered every row. Now both run this module: the RULES (split, label, memory)
// and the ROW MARKUP are one implementation, and each page passes its own class map so its
// stylesheet and its tests keep their names.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

const MAX_BUFFER_LINES = 200;

/** "⋯ 126 more lines · to line 132" — the range, not just a count, when the rows are numbered. */
function capLabel(hiddenCount, toLine) {
  return "⋯ " + hiddenCount + " more lines" + (toLine != null ? " · to line " + toLine : "");
}

/** The first `cap` items and the rest; no cap (or nothing over it) means everything is shown. */
function capSplit(items, cap) {
  if (!cap || items.length <= cap) return { shown: items, hidden: [] };
  return { shown: items.slice(0, cap), hidden: items.slice(cap) };
}

/** A `pre` part's lines: a trailing newline ends the last line, it is not an empty line after
 *  it — a 200-line output that ends in "\n" has 200 lines, and the label says so. */
function preLines(text) {
  const lines = String(text ?? "").split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/** The last rendered row's line number of a `num`/`diff` part, for the expander label. */
function toLineOf(part) {
  const rows = part.rows || [];
  for (let i = rows.length - 1; i >= 0; i--) {
    const ln = part.p === "num" ? rows[i][0] : rows[i][1];
    if (ln != null) return ln;
  }
  return null;
}

const escapePart = value => String(value ?? "").replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]);

/** Numbered source rows: `[line, html]` — the html is Rust-escaped with syntect spans. */
function numRowsHtml(rows, classes) {
  return rows.map(r => `<div class="${classes.row}"><span class="${classes.gut}">${escapePart(r[0])}</span><span class="${classes.code}">${r[1]}</span></div>`).join("");
}

/** Diff rows: `[kind, line|null, text]` — plain text, escaped here; the mark per kind is the page's. */
function diffRowsHtml(rows, classes, marks) {
  return rows.map(r => {
    const kind = r[0];
    const cls = kind === "ctx" || !kind ? classes.row : classes.row + " " + (classes[kind] || kind);
    const mark = kind === "add" ? marks.add : kind === "del" ? marks.del : marks.ctx;
    return `<div class="${cls}"><span class="${classes.gut}">${r[1] == null ? "" : escapePart(r[1])}</span><span class="${classes.mark}">${mark}</span><span class="${classes.code}">${escapePart(r[2])}</span></div>`;
  }).join("");
}

/** The memory key of one expander: the record's id and the button's ordinal within it. */
function capKey(recordId, ordinal) {
  return recordId + ":" + ordinal;
}

/** Remember a small expansion (a Set of keys); a large one is not remembered, by design. */
function rememberCap(open, recordId, ordinal, hiddenLines) {
  if (!recordId || ordinal == null) return false;
  if (hiddenLines > MAX_BUFFER_LINES) return false;
  open.add(capKey(recordId, ordinal));
  return true;
}

/** Whether this expander was opened before — by its own key, or the record's "every cap" key. */
function capOpenHas(open, recordId, ordinal) {
  return !!recordId && (open.has(capKey(recordId, ordinal)) || open.has(capKey(recordId, "*")));
}

/** The hidden lines behind an expander, from the control's own stamp or the hidden element. */
function hiddenLines(button, hidden, rowClass) {
  const stamped = Number(button && button.dataset ? button.dataset.capLines : NaN);
  if (Number.isFinite(stamped) && stamped > 0) return stamped;
  if (!hidden) return 0;
  const rows = rowClass ? hidden.querySelectorAll("." + rowClass).length : 0;
  if (rows) return rows;
  return (hidden.textContent || "").split("\n").length;
}

export { MAX_BUFFER_LINES, capLabel, capSplit, preLines, toLineOf, numRowsHtml, diffRowsHtml, capKey, rememberCap, capOpenHas, hiddenLines };
