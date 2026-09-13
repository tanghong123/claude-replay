# The pitch deck's demo (#208)

`../agent-monitor-pitch.zh.html` is an elevator pitch for agent-monitor in Chinese — what it is
for and what it can do, not how it is built (that is `../agent-monitor-deck.html`). Everything in
it, the 37-second video included, is generated from this directory.

```
./run.sh [port] [big-mb]      # store → monitor → tour → video → stills → deck
```

**Nothing here comes from a real session.** `make_store.py` writes the whole demo world by hand:
three agents (Claude Code, Codex, QoderWork), six sessions across five projects, and one
transcript of the requested size (200 MB by default, ~9,000 turns) for the performance shot. The
two live states a reader triages by — one session waiting on an answer, one still working — come
from stand-in processes: a shell whose `argv[0]` is `claude` and whose argv carries the session
id, which is what the monitor's process probe links a row to. No agent is run.

**The pieces.** `cdp.mjs` is a dependency-free DevTools-protocol client (Node's own `WebSocket`),
because no Playwright package is installed on the machine this was built on — the cached browser
binary and CDP are enough. `tour.mjs` drives the app shell, injects the Chinese captions as a page
overlay, measures the big session's open time and records a screencast. `measure.mjs` is the same
open, timed three times, for the deck's warm/cold numbers. `build_deck.py` writes the deck with
the stills inlined; `--inline-video` also inlines the video, for publishing the page somewhere
that has no file beside it.

**The numbers in the deck are measured, not claimed**, and they come out of the run: on a 36 GB
Mac, 200 MB / 8,991 turns opened in 5.1 s cold and 1.2–2.3 s warm. Re-running `run.sh` refreshes
both the video and the figures; if a figure moves, change the deck's prose with it.
