# agent-monitor over a slow link (#s274)

**Status: approved (Hong, 2026-10-27, #s275); stages 0, 1 and 2 built (#s274, #s276, #s277; stage 2
as designed in §6, Hong's option 1 on #s279, with the changes in §6.11).** Hong's direction, from a phone over the
tailnet: a server-side turn index, fetching only the records the view needs, and server-side search.

## 1. What it is for

A phone reads a session through `tailscale serve`, often on a weak link. Today the app shell
downloads **every record of the session** before it can do anything that looks past the screen:

- The open is tail-first since #314: the last 512 KB first, then the rest (the "head") behind it.
- Search, the search filter, the tool letters and counts, the compaction marks, deep links and the
  file roster all scan the whole array in the page. Until the head lands, they see only the tail.

The record logs on hong-devserver, 2026-10-27: 28 sessions; median 10 MB, the top tenth over 86 MB,
the largest 150 MB. Gzip sends about a third of that. On a phone that is minutes of transfer and a
large share of the tab's memory, for a reader who usually wants the last few turns, or one turn
they search for.

Stage 0 is done: the head comes in ranges of 1 MB that resume after a drop. Before that, any drop
restarted the whole head, and the phone never finished. That stage makes the current model work on
a bad link. It does not make it cheap.

## 2. The idea

The server already holds the records as one file per session: JSON lines, append-only, read by byte
range. What the page lacks is a map of that file. So:

1. **A turn index on the server.** One line per turn: its number, its kind (a prompt, a command,
   another session's message, a relayed message), its label (the prompt's first line), its time,
   and where it lives (its first and last record, and the byte range of those records in the log).
   It also carries what the page now computes by scanning: tool counts, where compactions are, and
   the files the session offered. About 100 bytes a turn: a 1,200-turn session is about 120 KB, or
   30 KB gzipped.
2. **Fetching what the view needs.** The page loads the index first, and lays out every turn at an
   estimated height. It then fetches records turn by turn, by byte range, for the turns at and near
   the screen, and more as the reader scrolls. A jump (the Turns pane, a deep link, a search hit)
   fetches that turn first. The tail-first open becomes the ordinary case: "the view is at the end."
3. **Search on the server.** The page sends the query and gets back the matching records' places
   and counts. It steps through those, fetching each hit's turn before it lands on it. The filter
   works the same way.

## 3. Stages

Each stage ships and is measured on its own.

1. **The index and the summaries.**
   - The server writes the index beside the records log as the log grows, and serves it.
   - The page uses it for what it now scans the array for: the Turns pane, the tool letters and
     counts, the compaction marks, the file roster.
   - The head is still read whole, in the background. Low risk, and it moves those features off the
     head.
   - Built (#s276): the sidecar is `records.index`, one line per record; the reply covers the
     records below the boundary the page names (`upto`), because a turn can straddle it. It serves
     every tail-first open, the desktop's too: here the head still loads whole for everyone, so D3's
     "remote first" is stage 2's to apply. There is no files pane, so "the file roster" is the
     offered paths a link resolves to, beside the artifact roster.
2. **Records by turn.**
   - The page holds placeholders for the turns it has not fetched, the way it holds the head today.
   - It fills them in place as their records arrive. The viewport engine already does this for the
     head (#314's landing), and this generalises it to any turn, anywhere.
   - The anchor rule is the one the head uses: the reader stays on the turn they are reading.
   - Far-away turns may be dropped again on a phone, to bound memory.
   - Designed in §6. Hong asked that it not break the virtual window, so §6 goes through the
     engine's invariants one by one.
3. **Search and filter on the server.** One matcher answers both the page's query grammar and its
   filter, over the same text the reader sees.

The classic page (`agent-replay --html`, `--dump-html`) is unchanged. It is local, and it stays the
reference the browser suite holds the app shell to.

## 4. Decisions for Hong

- **D1: order.** Recommended: 1, 2, 3 as above. Stage 1 is useful on its own and cheap. Stage 2 is
  the large change, in the viewport engine. Stage 3 needs stage 2, because a hit outside the fetched
  turns must be fetched before the page can land on it.
- **D2: one matcher or two.** Search's meaning lives in `shared/search.js` today: the prefix letters,
  `o(…)` and what text a record shows. The server needs the same answer.
  - Recommended: write the matcher in Rust, and keep the JS one for the classic page. A parity test
    runs both over the rendering audit's corpus and a set of queries, and fails on any difference.
  - The alternative, running the JS on the server, needs a JS engine in the monitor: too heavy.
- **D3: who gets the windowed mode.** Recommended: a reader over the network first (the same test as
  gzip: a `Host` that is not the loopback). The desktop keeps loading everything until stage 2 has
  run on the phone for a while. Then everyone uses it.

None of it changes the public contract: the CLI, `--json` and the files other tools read stay as
they are. The new routes are the monitor's own, for its own page.

## 5. Risks

- **The engine's invariants.** Filling turns anywhere, while the reader scrolls and the tail grows,
  is the hard part. The engine's history and sandbox (#197) are how a misplaced fill gets
  reproduced; each stage-2 case runs with the check mode on.
- **Two matchers drifting.** The parity test is the guard, and it runs in CI.
- **A live session.** The index grows with the log, under the same epoch. A rewritten log (a new
  epoch) makes the page start over, as it does today.

## 6. Stage 2, designed (#s277)

Hong's condition on stage 2 (#s275): "make sure you design #2 properly to avoid breaking the
virtual window rendering". The engine's model and its fourteen invariants are
`design/virtual-window-framework.md`; this section says what stage 2 changes in that model, and
for each invariant how it still holds. An independent review checked every claim here against the
code; its corrections are in. Nothing here is built yet.

### 6.1 What the reader sees

- The open stays as it is: the last 512 KB first, with the short "Loading earlier turns" card
  above it.
- When the turn index arrives (a few hundred KB; measured 259 KB, before gzip, for a 117 MB log),
  the card gives way to the whole session. Every earlier turn is there at once: its prompt, shown
  by its label from the index, and under it placeholders for the rest of the turn, each the size
  the page expects. The scrollbar is the session's whole length.
- What is on or near the screen is fetched first and filled in where it stands. A jump fetches its
  turn first.

**This reverses two rules of #314** (`design/tail-first-open.md` §3), on purpose:
- **The short card.** It won because a fling up into a tall blank would land on nothing, and then,
  when the turns arrived, somewhere else. Neither holds now. A placeholder shows its turn's prompt,
  so a fling lands on something to read, and a fill never moves the unit the reader is on (§6.3).
  The short card stays for the moment before the index arrives, and for a session whose index cannot
  be read.
- **"A jump never lands on the card."** A jump to a turn now lands at once on its placeholder (§6.5).

### 6.2 Placeholders: one for each unit the turn will have

The engine's items are units (a prompt, an answer, a process group), not records. A placeholder per
TURN would be one item that later becomes several: a prepend in the middle of the list, every fill.
Instead, the page lays out the units the turn WILL have, with the keys they will have, before any of
its records arrive.

- **The index reply gains facts for each record below `upto`:** its kind, whether an answer is
  commentary, and its id. These are what `buildUnits` groups by, so the page runs its own grouping
  rule over them and the placeholders are exactly the units the real records will make, keyed as
  they will be (`user:t12`, `process:b57`). There is one grouping rule, the page's; the server
  reports facts, never a grouping verdict. About 20 bytes a record: on the 117 MB log the reply grew
  from 259 KB to 479 KB before gzip (10,965 records).
- **The store still holds `PENDING` for every record not fetched**, as today, so nothing that scans
  the records sees anything new.
- **A unit is a placeholder until every record it spans has arrived.** Only then is it drawn,
  measured and learned from. There are no half-filled units. This covers the one unit filled from
  its end: the tail starts in the middle of a turn, so its leading records join the head's last
  unit, which stays a placeholder until the head's part of it arrives.
- **A placeholder is sized, never measured.**
  - One never filled is drawn at the applied estimate for its kind (framework §1.3). That is the
    number the sums already use for every unit not yet mounted, so mounting a placeholder corrects
    nothing.
  - One that was filled and measured keeps its height by key, as every unit does. If it is ever a
    placeholder again (§6.6), it is drawn at that height.
  - "Drawn at" counts what the engine's measure counts: the box plus its margins, including the
    margins the neighbour stamps add (`stampNeighbours`).
  - The engine neither measures a placeholder nor learns from it. That is the new seam,
    `placeholderAt(i)`, which `measureMounted` and `learn` skip.
  - The applied estimate moves in two places: in a measure (`settleEstimates`) and in the rest
    timer's estimates transaction. In both, the mounted placeholders are redrawn at the new height
    inside that same transaction. A mount whose range has not changed redraws nothing today, so this
    is engine code, not a page hook.
- **A finding the seam also fixes.** `tail-first-open.md` §3 says the pending card "has its own
  kindOf, which the estimator never learns from". The code does learn from it: `kindFor` falls back
  to `process` for a kind with no floor, and `measureMounted` learns every mounted item. Today that is
  one share of at least 84 px in the process mean, withdrawn when the head lands. Thousands of
  placeholders would swamp the mean.

### 6.3 A fill is a rewrite in place

Fetched records replace their `PENDING` slots, and record indices never move (#314). The units over
them keep their index and their key; only their content changes. To the engine a fill is two things
it already does all the time: the rewrite of the open turn at the tail (every second on a live
session), and the first mount of a unit it has never measured. There is no shift and no
translation of `P`.

The prepend (#314's `{from, shift}`) is used once: when the index replaces the short card with the
placeholders. That landing keeps #340's rule and waits for a still view, as the head's landing
does today.

**A records change must name the RANGE it rewrote.** Today it names only where the rewrite starts,
and the engine redraws every mounted unit after that point. That is right for the tail, and wrong
for a fill above the window: every 1 MB piece would redraw the whole window, losing the reader's
text selection, focus and scroll inside a code block, and drawing diagrams again. With the range,
a fill outside the window redraws nothing. The page's own per-record work goes by the range too:
the projection (which today rebuilds every unit after the change), the task and agent targets, and
the search's per-record sizes.

**A fill at or above the reader waits for a still view** (D5). A fill that would redraw a mounted
unit at or above `P` waits until #340's `whenStill` (no finger down, no recent input, no scroll
event for 200 ms, two reads of the offset agreeing) and no scrollbar thumb is held. Then it lands
in one transaction, placed before paint. A fill outside the window lands at once. (As built, any fill
that would redraw a mounted unit waits while the view moves, below `P` too: the engine's position
can be a scroll batch old while the reader moves, so "below the reader" is not known well enough.)

**How each invariant holds** (I-numbers as in the framework's §2):

| | invariant | through a fill |
|---|---|---|
| I1 | only the reader sets `P` | A fill sets nothing. `P` names a key the fill keeps, and the records transaction reads `P` before the sums move, as for any rewrite. |
| I2 | every write is `place()` | A fill is a `recordsChanged`, a jump is `jumpTo`. The page writes no offset. |
| I3 | the sums are a pure function | A placeholder counts at its kept height or the applied estimate, and is drawn at exactly that, margins included. The check mode gains one test: a mounted placeholder's measured height equals its height in the sums. |
| I4 | the applied estimate changes only in a transaction that holds `P` | Filled units teach the live mean; it is applied at rest (#194). The placeholders it resizes are redrawn inside that transaction, wherever it runs. |
| I5 | one share per record | A placeholder never learns; a whole unit learns once, by key. |
| I6 | pads before any layout read | Unchanged: fills mount through `mountRange`. |
| I7 | a change that moved the DOM is placed before paint | A fill at or above `P` lands only on a still view, in one transaction that redraws, measures and places in the same task. Nothing is written into a glide. |
| I8 | a change that has not touched the DOM may wait | A fill outside the window changes nothing in the sums (its units are placeholders before and after, at the same height) and, with the ranged rewrite, redraws nothing. |
| I9 | growth below `P` never moves a non-following reader | A fill below `P` is measured below it; nothing above `P` moves. |
| I10 | a jump lands on content | A jump to a turn lands at once on its prompt placeholder, which shows the prompt's label; the prompt unit carries the landing's row, so the landing hold (framework §4.10) keeps its top in place while the turn fills. A fill does not end the hold: only the reader's input, a scroll the engine did not make, or a reshape do. A jump to a RECORD waits for its unit (§6.5). |
| I11 | the record under `P` is mounted | Unchanged: a placeholder is an item like any other. |
| I12 | a rewrite that drops `P`'s key falls back | A fill keeps every key, by construction. The head's records can still be rewritten under the same epoch: a monitor restart resumes at epoch 1, and a client ahead of a log that was cut gets everything from 0 with no epoch change (`pull.rs`). So any pull that changes a record below `upto`, whatever its epoch, cancels the reads and starts the session over, and every fetched record is checked against the index's fingerprint (`h`) before it fills. Never a rename. |
| I13 | follow is dropped only by the reader | A fill never touches follow. A following reader stays at the tail while fills land above. |
| I14 | nothing is written under a drag | A fill at or above `P` waits while the thumb is held (above). Today nothing holds a records change under a drag, so this is new page code. |

**The cases the task named:**

- *The reader's anchor:* I1, I10, I12 above. The anchor's key survives every fill.
- *Estimates applied only at rest:* I4. Fills feed the mean; the sums take it only at rest, and the
  placeholders follow in the same transaction.
- *Nothing written into a touch glide (#372):* a fill at or above the reader waits for a still view,
  so it never needs the glide's absorbing. Absorbing fills mid-glide was weighed and is D5's
  alternative: the corrections of every fill in one glide add up in the top pad, which shrinks as
  the window moves up, so near the session's top a fling up into placeholders would jump.
- *A landing held until the view is still (#340):* kept for the one prepend, and widened to fills at
  or above the reader.
- *The identity shift (#165):* a queued prompt picked up rewrites the open turn at the tail, above
  `upto`; fills touch only records below it. Anything that rewrites below `upto` starts the session
  over (I12).
- *The tail growing while a middle turn fills:* two records transactions on disjoint ranges, each
  placing `P`. A following reader stays at the tail; any other reader stays on their unit.

### 6.4 What is fetched, and when

- **The reads** are `/records` byte ranges from the index's offsets: at most 1 MB each, in whole
  records, with #s274's resuming reader. One read at a time; a nearer need goes ahead of a farther
  one at the next read.
- **The order:**
  1. the turn a jump or a link names;
  2. the turns in the mounted window, then a screen beyond it either way;
  3. the rest, from the tail backwards (D4).
- **A piece is applied as it arrives, unit by unit.** Turns vary far more than their median suggests
  (measured on hong-devserver's 25 largest record logs):

  | log | turns | median turn | 90th percentile | largest turn | largest record |
  |---|---|---|---|---|---|
  | 150 MB | 377 | 31 KB | 1.3 MB | 6.3 MB | 0.68 MB |
  | 117 MB | 523 | 26 KB | 0.57 MB | 13.8 MB | 0.67 MB |
  | 92 MB | 378 | 31 KB | 0.61 MB | 9.9 MB | 0.68 MB |
  | 31 MB | 1,281 | 6 KB | 0.04 MB | 1.3 MB | 0.54 MB |

  18 of the 25 have a turn over 1 MB. Waiting for a whole 14 MB turn before showing its prompt would
  be a minute on a weak link. A turn is read from its first record, so the first piece shows the
  prompt and every unit it completes. A process group of several MB stays a placeholder until its
  last record arrives; its placeholder says how much of it has.

### 6.5 Search, the filter, links and the panes

- **The Turns pane** lists every turn from the index (stage 1). A tap jumps at once to the turn's
  placeholder (I10) and fetches it. #s276's held tap goes. Compaction marks work the same.
- **A jump to a record** (a link `#b57`, a search hit, a task or an agent) needs the record's row and
  the folds around it, which a placeholder does not have. So the page fetches its turn, says it is
  still loading, and jumps once the record's unit is whole: the reveal (folds and caps opened) runs
  then, on the real unit. The jump is dropped if the reader moves first. Today `landOnHash` marks a
  link landed when it jumps, so a reveal after a fill would never run; it is marked landed only once
  the unit is whole.
- **Links** find their record through the per-record ids. A nested id belongs to the record whose
  first id is the nearest at or below it.
- **A remembered position** names a unit key. The placeholders carry the real keys, so a restore
  finds its unit at once and fetches it.
- **The Agents pane:** the index also carries each record's sub-agent: `head.child_id`, or the
  session named in `head.child`, which is what `agentRecordTargets` reads. Its targets are then
  whole, and a tap is a jump to a record.
- **The Tasks pane** finds a task's record by matching text (`taskRecordTargets`), which an index
  cannot answer. A task whose record is not fetched has no target yet, and its row says it is still
  loading. Fetching the rest (D4) or stage 3 resolves it.
- **Search and the filter** read the fetched records only, as in stage 1, and the count says so while
  some are missing. Fetching the rest (D4) makes them whole; stage 3 makes them whole from the start.
- **The counts** (turns, tool letters and counts, artifacts, offered files) keep stage 1's rule,
  widened: the index answers for every record below `upto` and a scan only for those at or above it,
  whatever has been fetched. Never both for one record: that double count was the first finding of
  #s276's review.
- **Folds and Expand all:** a filled unit takes the current choices, as a unit arriving at the tail
  does.

### 6.6 Memory on a phone

With D4's first option the page ends up holding every record. A bound had to wait for stage 3:
search and the filter read the records in the page, and dropping some would have made them wrong.
Stage 3 (§7) answers them on the server, and #s285 built the bound below.

Designed with stage 2: the records of turns far from the reader (by bytes, not turns: say 16 MB kept
around `P`) go back to `PENDING`. Their units become placeholders again at their kept measured
heights, so a drop is a rewrite in place that moves nothing, and a later fetch is a fill. Only head
turns are dropped, whose offsets the index knows; never the live tail.

**As built (#s285)**, after Hong chose on #s290 ("Read up to about 16 MB, then only what I visit"):
- **The background read stops at the budget.** D4 reads the rest from the tail backwards. It stops
  before the turn that would take the head's reads past `KEEP_BYTES` (16 MiB of the log;
  `?keepBytes=` for a case). Reaching the budget drops nothing, so what the background has read is
  one unbroken run of turns behind the tail. A visit that takes the reads past the budget ends the
  background read too, so nothing let go for it is read again behind the reader. After that, only
  what the window shows or a jump names is read. A head under the budget is read whole, as before, and the head and its index then go.
- **Asks lapse.** After every transaction the page asks again for what the window shows and for the
  jump it still waits on (a held turn, record or search hit, a link on its way). Every earlier ask
  goes back to the background order first (`unwant`). So a turn the reader scrolled past is not read
  once the background read is over, and a jump they gave up costs at most the read already asked:
  the next fill lets go of a held jump the reader moved away from before its own transaction asks.
- **A read starts once every ask made with it is in** (a microtask): the store is idle when the
  reader asks, and a read started on the first ask read the window's farthest turn first.
- **The farthest go after each fill** (`dropFar`): while the reads hold more than the budget, the read
  turns farthest from the reader's unit go back to `PENDING` through the store's `drop`. Their units
  are rebuilt in place by the fill's own path (`Projection.fill`), as placeholders under the same keys.
  Their heights are kept by key, so the sums do not move and nothing mounted is redrawn.
- **What is never dropped:** a turn within twice the reach the window asks for (so nothing it asks
  for is dropped and read again; the reach is taken again before each drop), the read in flight, one
  with a piece still waiting to go in, and the head's last turn when the tail goes on with it (its
  last unit reaches into the tail).
- **What a dropped turn costs:** what any turn still on its way costs. The counts, search and the
  filter answer from the index and the server for every record below `upto`, so a drop changes none of
  them. The filter remembers every record it has opened under its tools (`filterSeen`), so a turn
  read again does not open a fold the reader closed, and nothing is redrawn for it.
- **A task's "Go to the turn"** reads its target from what the page holds only when that record is in
  the tail. The tail is always held, and it comes after every record of the head. Otherwise the button
  asks the server's search for the latest record of the head that names the task (`findTaskRecord`),
  then jumps there, reading the turn. It asks twice, as `taskRecordTargets` matches: for the subject,
  which Claude Code writes only where the task is created, and for `#<id>` as a whole word, which is
  how every TaskUpdate (`{taskId, status}`) is drawn. A reader who moved on while it was asked is not
  taken back. Without this, a task recorded in a turn never read said "still
  loading" for good, and one whose latest record was dropped pointed at an older one.
- **The bound** is the budget, plus whatever the window and its reach hold that cannot be let go. A
  single turn larger than the budget, read because the reader is in it, is held whole.
- **The cases** are `a_reader_across_a_network_keeps_about_its_budget_of_a_long_session`,
  `the_turn_the_tail_goes_on_with_is_never_let_go` and
  `a_task_recorded_in_a_turn_never_read_is_found_on_the_server`.

### 6.7 Who gets it

D3 as decided: a reader over the network (a `Host` that is not the loopback, the test gzip uses)
gets stage 2. The desktop keeps stage 1, the head read whole and landed once. Stage 1's path is
also what a reader over the network falls back to when the index cannot be read. `?windowed=1|0`
picks either path for a case.

### 6.8 What changes where

- **The engine** (`shared/virtual-window.js`):
  - `placeholderAt(i)`: never measured, never learned, drawn at its height in the sums, redrawn
    wherever the applied estimate moves;
  - a records change names the range it rewrote, and nothing outside it is redrawn;
  - the check-mode test of I3 above;
  - the history records a fill's range, so a saved export replays it.
- **The server:** the index reply gains the per-record facts (kind, commentary, id, sub-agent). The
  sidecar's version moves, so every sidecar is rebuilt once from its log (117 MB in 0.22 s, measured
  in #s276). No new route.
- **The page:**
  - the record store keeps a read queue in place of the one head read, cancelled by any change below
    `upto`, and checks each record's fingerprint;
  - the hold for fills at or above the reader: a still view and no thumb held;
  - the projection, the targets and the per-record sizes go by the rewritten range;
  - the placeholder's drawing;
  - the panes, links and counts of §6.5.
- **The classic page** is unchanged.

The review also found that stage 1, shipped, can splice a head over a log rewritten under the same
epoch (the I12 row): its record store drops a head in flight only on a new epoch. That is a bug of
its own, fixed apart from this design.

### 6.9 What proves it

Each browser case runs the app shell as a phone reading a remote monitor, with the check mode on and
`window.__viewportViolations` empty at the end.

1. The index lands. The short card gives way to the placeholders. A reader at the tail stays at the
   tail and following; a reader scrolled up keeps their record within 1 px.
2. A fill above the reader, inside the window, waits during an emulated iOS glide
   (`harness::touch_glide`), while a finger is down and while the thumb is held, then lands with the
   reader's record within 1 px.
3. A fill below the reader moves nothing.
4. A turn the reader jumped to keeps its prompt's top where it landed while the turn fills.
5. A fill outside the window leaves the offset, the reader's record and every mounted element as
   they were (the same nodes: nothing redrawn).
6. A Turns pane tap on an unfetched turn lands at once on its prompt. The reads show that turn's
   range first.
7. A link to a nested record in an unfetched turn lands on its row, with its folds open, once its
   unit is whole. A reader who moves first is not pulled back.
8. The tail grows and a queued prompt is picked up (#165) while a middle turn fills: both apply, the
   reader holds, follow is unchanged.
9. A 409 during a fill starts the session over, and so does a pull that rewrites below `upto` under
   the same epoch.
10. The counts are equal with the index held, part-filled and whole (#s276's comparison, widened).
11. A turn of several MB shows its prompt from its first piece.
12. Placeholders teach nothing: after a walk with many fills, the estimator's means equal those of
    the same session loaded whole.

Then:
- **Contract (node):** `buildUnits` over the index's facts equals `buildUnits` over the records,
  keys, kinds and ranges, over the rendering audit's corpus; the `placeholderAt` seam and the
  estimate redraw; the range of a rewrite.
- **Sandbox (#197):** a fill builder beside the prepend one, and a saved history of a walk with many
  fills that replays.
- **The iOS Simulator walk** over the slow proxy of #s274: upright and sideways, light and dark; a
  fling up through placeholders, a jump to an early turn, search with the keyboard up.

### 6.10 Decisions for Hong

- **The two reversals of #314** (§6.1) are part of approving this design.
- **D4: fetch the rest behind the reader until stage 3?**
  - Recommended: yes. Search, the filter and the Tasks pane become whole once the rest has arrived,
    as they do in stage 1. The page reads no more than stage 1 does, only in a better order.
  - Otherwise: fetch only what the reader visits. Search covers only those turns until stage 3.
- **D5: a fill at or above the reader while the view moves.**
  - Recommended: it waits for a still view (#340's rule, and the thumb released). During a fling the
    placeholders stay; they fill about a fifth of a second after the reader stops.
  - Otherwise: absorb it into the top pad mid-glide, as #372 does for a unit first mounted then. The
    content appears sooner, but near the session's top the pad runs out and the view jumps.

### 6.11 As built (#s277)

What changed between this design and the code, each decided in the build:
- **The record check is the facts, not a fingerprint.** A fingerprint per record would add 16 bytes to
  each of the reply's records; the facts already name each record's kind and id, and a fetched record
  that differs from them (a log rewritten under one epoch) starts the session over before it fills.
  With the I12 rule (any pull that rewrites below `upto` starts over, #s280) that is the same promise.
- **The pending card is a kind of its own** (`pending`, with its 84 px as its floor) rather than a
  placeholder: it is short by design and measured, and now teaches only its own mean.
- **A late measure mounts the window where the reader is.** The observer reports a resize in a frame;
  placeholders refitted to a moved estimate made a report that arrived after the reader had scrolled
  into a pad common, and the measure transaction did not remount (I10, I11). It now does, without a
  placement, as a yielded correction does (#372).
- **A process group's placeholder says how much of it has arrived** ("Loading — 12 of 40 steps"); a
  prompt's shows its turn number and label.
- **A group of steps that reaches into the tail is the live turn's.** A tail that starts in the middle
  of a long turn holds no prompt, and its steps join the head's group, keyed by its first record, in
  the head. That group is drawn with what has arrived (its count says how many earlier steps are still
  loading), measured as any unit is, never as a placeholder: drawn as one, it hid the live tail until
  the head's part of the turn arrived. When the index lands, the page tells the engine the group's new
  key (`renamed` beside the prepend's `shift`), so a reader inside it keeps the step they were reading;
  the shift is found by the unit's first record, not its old key. A prompt still on its way stays a
  placeholder.
- **Every piece is read as a stream**, keeping each record it finishes, as #s274's head reader does.
- **A jump decides whether it must wait from its unit** (it owes records, read or not yet taken), and
  a link waiting on a unit gives way to a reader who moves, whether or not its record has arrived. The
  desktop's links keep stage 1's rule, waiting for the head.
- **Agents still on their way get their targets from the index**, so an agent's jump reads its turn
  and lands; a task's says "still loading" while the head is out.
- **The task and agent targets are recomputed on each fill** (a scan of the records, cheap beside a
  render); the projection and the redraw go by the run, as §6.3 says.
- **The sandbox replays a records-by-turn walk** (#s281): an export names where its tail began and
  when it filled (`Export::tail_start`, `Export::fills`); the replay opens the synthetic session records
  by turn with its tail there and lets in one queued piece of the head per recorded fill, on the
  recorded timeline. `fill-walk-app.json` (a walk up through 33 fills, then a jump) replays within 2
  turns, with no violation (`sandbox_fill_walk_app_replays`; `sandbox_record_a_fill_walk` records
  another with `CR_RECORD_FILL_WALK=<file>`).
- **The cases** are `tests/records_by_turn.rs`: the index lands and nothing moves the reader (a far fill
  redraws no mounted element), the tail stays followed through every fill, a fill at or above the
  reader waits through a glide and under a finger, a turn tap lands at once and is read first, a link
  lands on its row unless the reader moves, the tail grows and a queued prompt is picked up while the
  head fills, a stale epoch starts over, a turn of many records shows its prompt from its first piece,
  a placeholder is never measured and moves no mean, and a tail with no prompt stays drawn while its
  turn fills, its reader on their step. Removing the fill hold fails the third.

## 7. Stage 3, the build plan (#s278)

Stage 3 follows the design Hong approved (D1, D2): search and the filter answered by the server for the
records the page does not hold, with one Rust matcher held to the page's by a parity test. This is the
plan the build follows, decided in the build; nothing here changes what a query means.

### 7.1 The page keeps the grammar; the server matches

The box's grammar (`design/in-session-search.md` §9: the prefix letters, `o(…)`, the escape, the tool
letters) stays the page's. The page already reads the box into a structured query (`splitQuery`,
`resolveTools` with the session's letters), and sends THAT: the lowercased needle, whole words, the
wanted class mask, and the resolved tools as `[{name, prefix}]`. So the server needs only the matching
half of `shared/search.js`: `directMask`, `ownTextParts`, `recordTextParts`, `stripTags`, `wholeAt`,
`countOcc`, `toolMatches`, `partWanted`, `countRecord` and `recordHasTool`. It is ported literally,
traps included (five sequential entity replaces, so `&amp;lt;` reads `<`; JavaScript truthiness on
head fields; `String(undefined)` for a part with no text; the whole-word classes `\p{L}\p{N}\p{M}_`
from `regex-syntax`'s Unicode tables, already shipped through syntect; the unscoped total counted over the whole text,
not summed from the parts). Where the two pages strip HTML differently, the server matches the app
shell's (`stripTags`): stage 3 serves only the app shell.

### 7.2 The route

`/search?session=&epoch=&upto=&q=<the structured query, JSON>` answers for the records below `upto`,
exactly as `/turn-index` does: 409 on a stale epoch, behind `ensure_known`, through `prepare_reply`.
The reply is `{t: "search", epoch, upto, records: [[r, hits], …], counts: {u, a, t, o}, total}`; a
query with tools and no text (the filter) answers the records holding such a call. **It quotes no
text**, so nothing it sends needs masking; but for a client that is sent masked records (#365) it
matches each record as masked, so its counts are over what the page will hold and cannot read a
masked secret back a character at a time. The query is never logged. It reads
the log outside the session lock, so a search never holds the page's polls, and asks the epoch again
after the read. Measured on the 117 MB log (`CR_SEARCH_PROBE`, release build): 0.21 to 0.27 s for a
text query over all 10,965 records, 0.09 s for the filter's tools alone — well within an Enter, so the
server keeps no lowercased text.

### 7.3 The page

- While a records-by-turn head is out, the page counts the records at or above `upto` itself and asks
  the server for the ones below it, whatever has been fetched (the panes' rule, `scanFrom`), then sums.
  The answer lives as long as the index does: dropped when the head is whole, on a resync, on a reset.
- Search then runs on Enter, as one over the live limit does (`LIVE_SEARCH_LIMIT`, #104): no round
  trip per keystroke over a slow link.
- A step onto a hit in a turn still on its way reads that turn and lands once its unit is whole
  (`jumpOrFetch`, `search`); its marks appear then.
- The filter: a record still on its way is in the filter's matches when the server says it holds such a
  call. A placeholder is never hidden or dimmed (it holds nothing to filter, and its height is the
  sums'); the filter applies to its unit once it fills.
- The desktop and the classic page are unchanged: they hold every record.

### 7.4 What proves it

- **Parity:** a `cargo test` in `claude-replay-html` renders the rendering audit's corpus to records,
  runs the Rust matcher over a query set, and has node run `shared/search.js` over the same records
  and queries; any difference fails. It runs where `ui_contract` runs (skipped without node, run in
  CI). The query set covers every prefix letter and combinations, `w`, `o(B)`, `o(Bash,Read)`,
  `o(mcp__x__*)`, the escape, a needle too short, CJK and accented needles, a head field against body
  parts, nested blocks, punctuation at word edges.
- **The route:** its 409, its masking (nothing quoted; a masked client never finds a masked secret),
  the boundary `upto`.
- **Browser cases** (`tests/records_by_turn.rs`): with the head held, a search counts the whole
  session, equal to the count once it is whole; a step to an unfetched hit lands on its mark once the
  turn arrives; the filter's matches include records still on their way, and placeholders stay drawn.
- The whole local suite and a Simulator walk (search with the keyboard up while the head is out).

### 7.5 Not in stage 3

- **Dropping far turns on a phone** (§6.6) becomes possible once search no longer needs every record in
  the page; it is a change to the store and the projection of its own, filed as its own task.
- Fetching the rest behind the reader (D4) stays: with search on the server it could stop, which is a
  choice for Hong once stage 3 has run on the phone. (He chose on #s290: it stops once the page holds
  about 16 MB, §6.6.)

### 7.6 As built (#s278)

- **A step while the server's answer is out waits for it** (`stepWhenFound`, tied to that answer), the
  search's or the filter's, unless the reader moves, types or starts another search first: a step
  taken before it would have stepped through the tail's hits alone. The hits an answer brings can come
  in front of the one landed on, so the current hit is found again by its record.
- **The page's filter is the shell's own chain** (`filterChain`: a tool-call kind whose `tool`, else its
  head's `name`, answers the tools), not `recordHasTool`, which only the classic page uses; the server
  ports the shell's.
- **Measured** on the 117 MB log: 0.21 to 0.27 s a text query, 0.09 s the filter, read outside the
  session lock; no text cache.
- **Text and the filter keep an answer each** (one slot for both lost an answer whenever the two
  crossed), and the filter is asked with text up too, so it is whole the moment the text goes. A
  query whose answer is still out keeps the count at "Searching…", never the tail's part of it. A
  failed ask is tried again every 3 s — a 409 too, since a session the server's cache let go answers
  it until the next pull brings it back, and a log that really moved starts the session over — and
  after five the count says "earlier turns not searched"; the next search started asks again. The
  server answers 503 for a log it cannot read.
- **Any search a control starts submits the box as it stands** (Enter, a scope row, whole words, a
  chip's ✕), so its answer is counted when it lands; only a fill or an answer arriving leaves text that
  was typed and not entered alone. The step arrows and their keys enter typed text first, as Enter
  does: stepping the last query's hits under new text read as stepping the new one, and over a slow
  link those hits were the tail's alone.
- **A masked client is searched as masked** (the review's finding): the counts over unmasked text would
  have differed from the page's own and answered, a query at a time, what the mask hides.
- **The parity test** runs 4,165 queries; a word class from `char::is_alphanumeric` and the entities
  decoded out of order each fail it.
