// The tool head both pages compose (#117, design/rendering-parity-audit.md row 3.4): the
// display-name rule and the STATE a head's chips carry — exit code, status word, duration and
// line count. The server ships chips as text ("exit 1 · 2.50s", "declined · 42ms", "12 lines",
// "+3", "−1", "launched") with a class for failure presentation; the classic page shows the
// chips as they are and the app shell folds them into one state pill. Until now the app shell
// derived that state by regex over the chip text (/fail|error/ and /running|active/ — the
// second matched nothing the server writes) and lost the exit code the moment it said
// "failed". Now both pages read the head through this module: the words are pinned HERE, from
// the emitters (present.rs tool_execution_summary and spawn_chip, model.rs done_verb, mod.rs
// chips), and a head's state, exit and duration are facts rather than a pattern match.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

/** Claude Code labels Edit/MultiEdit as Update; every other tool keeps its name (present.rs
 *  display_name, which the server already applies — this is the same rule on the page, so a
 *  head that reaches a page unnamed by the server still reads the same). A null-prototype map:
 *  a tool called "constructor" or "toString" must not pick up an Object.prototype member. */
const DISPLAY_NAMES = Object.assign(Object.create(null), { Edit: "Update", MultiEdit: "Update" });

function displayName(name) {
  const n = String(name ?? "");
  return DISPLAY_NAMES[n] || n;
}

/** The status words the server writes into a chip, and what each means for the head.
 *  A head carries NO liveness: "launched" is the launch EVENT of an async spawn
 *  (present.rs spawn_chip — it reads "launched" whatever the spawn's status, because the
 *  terminal verb arrives on a separate AgentDone record), so it must never be read as
 *  "still running" or every finished sub-agent in a closed session looks in-flight. */
const FAILED_WORDS = new Set(["failed", "killed"]); // failure presentation, each keeping its word
const REFUSED_WORDS = new Set(["declined", "cancelled"]); // failure presentation, each keeping its word
const DONE_WORDS = new Set(["completed", "done", "finished", "stopped", "launched"]);
const EXIT = /^exit (-?\d+)$/;
const LINES = /^(\d+) lines$/;
/** format_tool_duration: "42ms", "2.50s", "7s", "1m 5s", "12µs", "40ns". */
const DURATION = /^\d+(?:\.\d+)?(?:ms|µs|ns|s)$|^\d+m \d+s$/;

/** The chips' text as one line — what a completed pill shows. */
function chipText(chips) {
  return chips.map(c => (c && c.x) || "").filter(Boolean).join(" · ");
}

/** Read a record head: its display name, target and chips, and the facts the chips carry —
 *  `state` (failed | completed, or null when there is no chip at all, so a chipless head keeps
 *  its renderer's own word), `status` (the wire word), `exit`, `duration`, `lines`. */
function toolHead(head) {
  const h = head || {};
  const chips = Array.isArray(h.chips) ? h.chips : [];
  let exit = null;
  let duration = "";
  let status = "";
  let lines = null;
  let failed = false;
  for (const chip of chips) {
    if (chip && chip.c === "fail") failed = true;
    for (const piece of String((chip && chip.x) || "").split(" · ")) {
      let m;
      if ((m = EXIT.exec(piece))) {
        exit = Number(m[1]);
        if (exit !== 0) failed = true;
      } else if (DURATION.test(piece)) duration = piece;
      else if ((m = LINES.exec(piece))) lines = Number(m[1]);
      else if (FAILED_WORDS.has(piece) || REFUSED_WORDS.has(piece)) {
        status = piece;
        failed = true;
      } else if (DONE_WORDS.has(piece)) status = piece;
    }
  }
  const state = failed ? "failed" : chips.length ? "completed" : null;
  return { name: displayName(h.name), target: h.target || "", chips, text: chipText(chips), state, status, exit, duration, lines, failed };
}

/** The one-pill label: a failure keeps the server's own word (failed, killed, declined,
 *  cancelled — "failed" when only a non-zero exit says so) and names its exit code; anything
 *  else shows the chips' text. */
function stateLabel(th) {
  if (th.state === "failed") {
    const word = th.status || "failed";
    return th.exit != null && th.exit !== 0 ? `${word} · exit ${th.exit}` : word;
  }
  return th.text;
}

/* ── the head's click cycle (#129) ────────────────────────────────────────
 * A tool head shows its target on one line: for a Bash call that is the command, and a long
 * command is clipped. The output was the only thing a click could reveal, so the command a
 * reader wanted — the flags, the heredoc, the far end of a pipeline — was unreachable.
 *
 * The owner's cycle, from folded: one click opens the OUTPUT, the next opens the COMMAND
 * (output stays), the next folds the command back (output stays), the next folds the output.
 * Four clicks, and the reader passes through "output only" on the way in and on the way out —
 * which is the state they want most of the time.
 *
 * A head with nothing clipped keeps the plain two-state toggle: an extra click to close a fold
 * whose target already fits would be a tax on every short call, and row density is what makes a
 * long transcript readable. */
const HEAD_STEPS = [
  { open: false, full: false },
  { open: true, full: false },
  { open: true, full: true },
  { open: true, full: false },
];

/** Where a click takes a head. `clipped` is the page's measurement — does the target overflow
 *  its one line — because only then is there a third state worth stopping at. */
function nextHeadStep(step, clipped) {
  const at = Number(step) || 0;
  if (!clipped) return at === 0 ? 1 : 0;
  return (at + 1) % HEAD_STEPS.length;
}

/** What a step shows: the output open, and the target at full length. */
function headStepState(step) {
  return HEAD_STEPS[Number(step) || 0] || HEAD_STEPS[0];
}

/** The step a head is at, given what it is showing — for a head whose state a page restored
 *  (a remembered fold, an authored-open block) rather than clicked into. */
function headStepOf(open, full) {
  if (!open) return 0;
  return full ? 2 : 1;
}

export { displayName, toolHead, stateLabel, HEAD_STEPS, nextHeadStep, headStepState, headStepOf };
