# The viewport history: an hour of what the reader did and what the engine saw (#197)

Status: design, 2026-09-13. Points 3 and 4 of the owner's directive (recorded on #195): "enhance the
diagnostic approach, keeping a limited history of user actions and corresponding app states (one
hour is sufficient), so if a user reports an issue, you should be able to tell exactly the sequence
of events (you may want to document the position of the tail, this way you can simulate growth in a
sandbox)" and "you should still try to reproduce bugs with shorter synthetic transcripts, otherwise,
the debugging process will just take too long." The engine it instruments is
`design/virtual-window-framework.md`; this is a different instrument from the engine's invariants
(§4.12 there checks what the engine promised; this records what happened), so it has its own page.

## 0. The one sentence

The engine keeps, always, the last hour of three streams — the reader's actions, its own state after
every transaction, and the shape of every records change — built from what it already knows,
exportable without a session's content, and replayable in the harness against a synthetic transcript
with the same shape.

## 1. What exists

- **The trace (#192).** `trace(event, fields)`: off unless `?trace=viewport` or
  `localStorage.viewportTrace`; a 500-entry ring at `window.__viewportTrace` and one `console.debug`
  line per entry. An entry carries the geometry read LIVE — `frame.scrollTop()`,
  `frame.scrollHeight()` — which is why it is opt-in: two layout reads per seam, on every seam. It
  records transaction summaries under their cause and the seams inside them (`place`, `scroll`,
  `scroll:own`, `measured`, `reshaped`, `estimates:*`, `tail:deferred`, `rest`).
- **The check mode (#196 stage 6).** `violations`, a ring of 50, always on.
- **Reader input reaches the engine at one place.** `noteIntent` (the constructor's capture
  listeners: pointerdown, pointermove with a button, wheel, touchstart, touchmove, keydown outside a
  field) → `markIntent(stamp)`; the scrollbar thumb through `beginDrag`/`endDrag`; commanded moves
  through `command(cause, position, options)` — `jump`, `move`, `reveal`, `hold`, `follow`; a page
  reshape through `readerReshaped()`. A fold toggle or a control is the page's handler, then
  `rerender()`: the engine sees the transaction, not the click.
- **Records reach the engine at one place.** `recordsChanged(mutate)`; the mutate returns the first
  rewritten index (`Infinity` when the batch only appended); the count before and after are the
  engine's own.

The gap the directive names: a report from a real session carries the trace only if the reader had
turned it on beforehand, the trace has no actions in it (a wheel is visible only as the scroll
verdicts it caused), and nothing in it says what the records did — so a sequence of events cannot be
told from it, and growth cannot be simulated from it.

## 2. The model: three streams, one clock

The history is the engine's, one implementation for both pages, **always on**, and **bounded by
time**: an entry older than `historyMs` (a constructor parameter, default 3 600 000) is dropped when
a new one is pushed; a count cap per stream is the backstop, never the bound. Every entry carries
`t`, `performance.now()` rounded to the millisecond, so a replay has the timing between events.

### 2.1 Actions — what the reader did

| from | entry |
|---|---|
| `noteIntent` | `{kind: "wheel", dy, n}` — consecutive inputs of one type within 300 ms COALESCE: `n` inputs, `dy` the summed `deltaY`, `t` the first, `until` the last. A wheel gesture is one action, not forty. `{kind: "key", key, n}`, `{kind: "pointer", n}`, `{kind: "touch", n}` likewise. |
| `beginDrag` / `endDrag` | `{kind: "drag", ms}` — one entry, closed at the end. |
| `command` | `{kind: cause, index, key, top, intent, smooth}` — the move the page asked for, with its target. |
| `readerReshaped` | `{kind: "reshaped"}` |
| the page, `noteAction(kind, target)` | `{kind, target}` — a fold toggle (`fold`, the record key and whether it opened), a control (`control`, its id), a filter or a search step. The page names what only it knows; the engine timestamps and stores. |

### 2.2 States — the engine after each transaction

The summary `transact` already assembles for the trace — `cause`, `p0`, `anchor`, `placed`, `range`,
`fresh` — is built ONCE and pushed to the history unconditionally; the seam entries and the console
line stay behind the trace flag. Each state carries, with **no layout read the engine did not
already make** (the #192 lesson, now the cost rule):

- `lo`, `hi`, `count`, `following`, `dragging`, `position` (`source:key|index`), `pending`;
- `pads` (the values `updatePads` wrote), `sums` (`prefix[count]`), `estimate` and `live` (the
  applied and the live mean of the default kind — model values);
- `top`: the engine's BELIEF of the offset — `position.at` when the position carries the offset it
  was read or written at; else what it wrote (`wrote`, or `wrote.to` while a smooth write travels);
  else `topSeen`, the last offset `onScroll` read for its own classification. Never a fresh read.
- `turn`: the turn of the record under `P` (`describeAt(index).turn`, §3) — the vocabulary the
  scenarios already compare in (`turn_at_top`, `at_tail`), and the one that means the same thing on
  both pages (§2.4).

### 2.3 Deltas — the shape of every records change

One entry per `records` transaction: `{count0, count1, from, kinds, tail0, tail1}` — the count
before and after; `from` the first rewritten index (`from < count0` is a rewrite — a queued prompt
picked up, a provisional turn replaced; otherwise an append); `kinds`, the kinds of `[from, count1)`
as the page describes them (the final records array shows only final kinds, and a rewrite CHANGES
the kind at an index — without this a pickup cannot be rebuilt); `tail0`/`tail1`, the last record
before and after: `{index, height, kind}` with `height` the measured height or `null`. Whether the
open turn GREW is derived by the sandbox from consecutive tail snapshots (the same index, a larger
measured height), never written back onto a past entry by a later measure.

### 2.4 The two pages index different things

The shell's engine index is a UNIT (a process group spans several records); the classic page's is a
block. A history keyed by engine index therefore describes two different arrays for the same
transcript, and a cross-page diff on `lo`/`hi`/`count` is meaningless by construction. So: the
export's session shape is RECORD-level, from the page's own records array, with the map from engine
index to record range beside it (`units: [[from, to], …]` on the shell; the identity on the classic
page); and the replay diffs by TURN, the way the scenarios do.

### 2.5 Cost

No layout reads (§2.2). One small object per entry; coalescing keeps the actions stream at the
number of gestures. Caps: 2 000 actions, 4 000 states, 2 000 deltas — a live session at one
transaction a second is 3 600 states an hour, so the time bound is what normally applies. Worst case
under two megabytes.

## 3. The seams

| seam | who | contract |
|---|---|---|
| `historyMs` | construction | the bound; default an hour; a scenario passes a small value to prove it |
| `noteAction(kind, target)` | page → engine | one entry in the actions stream, stamped now |
| `describeAt(i)` | page → engine | `{kind, turn, from, to}` for engine index `i`: the record kind(s) the page's own vocabulary uses, the turn, and the record range the index covers (default: `kindOf(i)`, no turn, `[i, i]`) |
| `exportHistory()` | anyone → engine | the export object (§4) |
| `window.__viewportHistory` | the console | `{actions, states, deltas, export()}` — `copy(__viewportHistory.export())` pastes it into a bug |

## 4. The export

One control on both pages — the classic topbar beside the theme button, the shell's Reading popover
— saves `viewport-history-<page>-<stamp>.json`; the console does the same through
`window.__viewportHistory.export()`. The file:

```
{ format: "viewport-history/1", page: "classic" | "app", version, exported: <ISO>, elapsed: <ms>,
  frame: { clientHeight, overscan, slacks, userIntentMs, floors, historyMs },
  session: { count, records: [[kind, height | null, turn | null], …], units: [[from, to], …] | null },
  actions: [...], states: [...], deltas: [...], violations: [...] }
```

Sessions are private: the export carries kinds, heights, indices, turns and timings — no text, no
ids, no path, no session id, and the filename carries none either. An export committed as a fixture
(§5) is checked for exactly that.

## 5. The sandbox

`claude-replay-browser-tests/tests/harness/history.rs`:

1. **`Export`** — the file, deserialized.
2. **`synthetic(&Export) -> String`** — a transcript with the same record kinds and heights: the
   builders exist (`user_at`, `assistant_at`, `tool_result_lines`, `pasted_image_sized`, …); the
   text length for a height comes from a calibration table (px per line at the harness width, the
   block's chrome) and is verified ONCE in Chrome — mount the synthetic session, read the measured
   heights, report the residual per kind; a residual beyond a line is the calibration's bug, not the
   case's.
3. **`growth(&Export) -> Vec<String>` + timings** — the delta timeline as a `LiveGrowth` script:
   each delta an append of `kinds` records; a rewrite is a queued record followed by its pickup (the
   parser rewrites, as it does in production); the intervals are the recorded ones.
4. **`replay(&Export, Surface) -> Vec<State>`** — drives the recorded actions with their timing
   (`scroll_by` for a wheel, `key` for a key, `jump_to_turn` for a jump, `open_last_fold`/the fold
   key for a fold) on either surface and returns the engine's states; `diff(&recorded, &replayed)`
   compares by turn and by `following`, step by step, and names the first divergence.

A report is then a reproduction: export → synthetic + growth + replay, and a SHORT one — the
synthetic transcript has the height profile, not the content, so it is as long as the window it took
to show the bug. The two #194 probes (`tmp_walk`, `tmp_runaway`) become the first exports and the
first sandbox cases.

## 6. Held by what

- **The node contract**: the history push path contains no `frame.scrollTop()`, `scrollHeight()` or
  `getBoundingClientRect` call (the cost rule made checkable); `historyMs` is a constructor
  parameter with the hour as its default; both pages call `noteAction` from their fold handlers
  (`toggleFold` on the classic page, the `data-process-toggle` branch on the shell) and their export
  control; the export's top-level keys.
- **A scenario on both surfaces**: with a small `historyMs`, entries age out; after a wheel, a jump,
  a fold and a live delta the three streams hold what happened in order, the delta's `from` and
  `kinds` describe the batch, the export has the format and no content.
- **The fixtures**: the probes' exports, identifiers stripped, replayed by the sandbox on both
  surfaces with the diff named.

## 7. Stages

- **A** — the engine's history, the cost rule, the seams, the controls, the pins, the scenario,
  CLAUDE.md (how to read and replay a history; the rule that a viewport bug is reproduced from the
  shortest synthetic transcript that shows it before it is fixed). A release.
- **B** — the sandbox helper, the probes as exports, the replay probe. A release; `#197` done after
  B.
