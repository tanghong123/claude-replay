# agent-monitor over a slow link (#s274)

**Status: proposed (2026-10-27).** Hong's direction, from a phone over the tailnet: a server-side
turn index, fetching only the records the view needs, and server-side search. Nothing here is built
yet except stage 0.

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
2. **Records by turn.**
   - The page holds placeholders for the turns it has not fetched, the way it holds the head today.
   - It fills them in place as their records arrive. The viewport engine already does this for the
     head (#314's landing), and this generalises it to any turn, anywhere.
   - The anchor rule is the one the head uses: the reader stays on the turn they are reading.
   - Far-away turns may be dropped again on a phone, to bound memory.
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
