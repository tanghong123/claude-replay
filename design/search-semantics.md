# Search, filter, and the question of how many boxes

*Survey and recommendation, 2026-09-06, for review and comment. Follows `one-engine-two-pages.md`
and `outline-drawers.md` in the same series: state what is there, draw the arithmetic, name what
it costs, and ask the questions that change the build. Nothing is built yet.*

---

## What was asked

> "Survey what the two search boxes in the classic app do, as well as the tools filter and the
> search filter. Then come up with your recommendation of a cohesive design of the search
> functions. 1) does it make sense to have two or just one search box? Any precedence of apps
> showing two search boxes instead of one? 2) The few usage cases of search today: a) filter/find
> sessions by agent type or session name; b) simple text match in blocks, filtered by block types;
> c) go through individual tool types. b) and c) has some overlap in the filter conditions but the
> filtering keys are different, b) filter on a set of message types, and c) only filter on one
> type. 3) balance cohesion of the semantics, but don't expand the backend capability too much —
> a clean semantics design that may require a complete overhaul of the backend search
> implementation may not be the direction to go."

And, mid-survey:

> "Search for strings across sessions would be a welcome addition — I have found myself needing to
> try the same search in multiple sessions because I don't remember in which session I have
> applied a particular technique."

---

## Two corrections to the premise, before anything else

**There is only ONE in-session box on each surface.** `.navigator-find` / `.navigator-searchbox`
in `reference.css` are dead CSS: no such element exists in `reference-shell.html` or the demo. The
two boxes a reader sees on the app shell are the **⌘K palette** and the **header search** — a
different pairing than "two transcript searches", and it matters, because they answer different
questions.

**"One box" was decided once already, and shipped.** `design/monitor-v2.md`, in the scout notes:

> **One search box is federation, not a merge.** The rail searches the session INDEX; the view
> searches the record stream (`searchNeedle`/`searchScope`). One input over two result kinds;
> mostly UX design, little architecture.

And v2 built it. `PageChrome::host_search` (`html_export/mod.rs:1377`) hides the page's own box
while leaving it as the thing that runs the search — the test pins the intent: *"the page's own
search box is hidden, not removed — it is still what runs the search"*. So the merged box is not a
proposal. It is a working mechanism in the product, and this document's job is to say what it
should federate over, not whether federation works.

---

## The survey

Four controls, and they disagree about nearly everything that matters.

| | **session finder** | **transcript search** | **tool filter (classic)** | **tool filter (app shell, #133)** |
|---|---|---|---|---|
| where | rail `#search` · sidebar ⌘K | `#q` · `#transcriptSearchInput` | `Filter ▾` → `#toolmenu` | the funnel → `navigatorOptions` |
| matches | `name + project + agent`, one substring | a record's **readable text** | a record's **properties** | a record's properties |
| keys | none — one blob | scope classes `u a t o b r e`, **multi-select** | one tool display name **or** one kind, **single-select** | tool names, multi-select |
| non-matches | **hidden** | **nothing** — marked only | **hidden, zero height** | **nothing** — marked only |
| stepping | — | ⏎ / ⇧⏎ / ▲▼ | ‹ › and `n`/`N` | ↑↓, shared with search |
| count | — | "12 hits in ua" | "3 of 17" | "2 matches" |

The four rows in bold are the whole problem. **The same word, typed into different boxes, means a
different kind of thing** — and the same act (excluding what you don't want) is expressed once as
a cut and once as a highlight.

### What a transcript search matches, precisely

This is written down and worth keeping (`shared/search.js:1-6`):

> A record's searchable TEXT is what a reader can see of it: its head's summary, badge, preview,
> name, target and attachment name, then its body parts — markdown with the tags stripped,
> pre/note text, numbered source lines, diff lines — and the same for every record nested in it.
> **Not its JSON: a query that is only a field name finds nothing.**

There is already a query grammar, shared with the TUI's `/`:

```
   uatobrew:needle
   │││││││└─ w  whole words (a MODIFIER, not a class — `w:foo` is unscoped, whole-word)
   ││││││└── e  edits / writes
   │││││└─── r  reads
   ││││└──── b  bash output
   │││└───── o  all tools
   ││└────── t  thinking
   │└─────── a  agent replies
   └──────── u  your turns

   order-free (`aut:` ≡ `uat:`) · a repeated letter is a word, not a scope
   a leading `:` escapes a scope-shaped literal · `+` still parses
```

So **case (b) is already built**: "text match filtered by a set of message types" is `ubr:needle`,
and the scope really is a set — a bitmask, OR'd. The funnel menu and the typed prefix are one
state, because the box is the single source of truth.

### What the tool filter matches, precisely

```js
// export.js:1714 — properties, never text
return !!((want.tool && b.tool === want.tool) ||
  (want.toolPre && b.tool && b.tool.indexOf(want.toolPre) === 0) ||
  (want.kind && b.kind === want.kind && !b.tool && isFoldRec(b)));
```

`var filter = null; // active tool-use filter (tool display name), or null` — **one** name, and
re-selecting it clears. Case (c), exactly as described.

---

## The seam nobody can cross

The two controls partition one conceptual space along **different cuts**, and neither can reach
the other's half:

```
                    ONE TOOL NAME          A CLASS OF RECORD
                    (Bash, Read,           (tools, thinking,
                     mcp__x__y)             your turns, replies)
                  ┌──────────────────────┬──────────────────────┐
   BY PROPERTY    │  tool filter  ✓      │  filter: kinds only, │
   (no text)      │  (single-select)     │  and NOT u or a  ✗   │
                  ├──────────────────────┼──────────────────────┤
   BY TEXT        │  nothing  ✗          │  search scope  ✓     │
                  │                      │  (multi-select)      │
                  └──────────────────────┴──────────────────────┘
```

- The filter **cannot select user or assistant messages at all** — `isFoldRec` (`export.js:536`)
  excludes `user`, `assistant`, `attachment`, `queue`, so they never become menu rows.
- The search scope **cannot name one tool** — only the class `o`. There is no way to say
  "occurrences of `needle` in `mcp__foo__bar` calls".
- So (b) and (c) do not *overlap*. They are two disjoint quarters of one control that was never
  designed as one.

**The missing term is a value, not a feature.** The scope language has classes; it has no way to
say *which* tool. That is the smallest thing that would make (b) and (c) one language.

---

## They break each other today

Not a design objection — a set of live bugs the survey confirmed in code. They matter here because
several of them **only exist because the two controls are separate state**, and a cohesive design
deletes them rather than fixing them.

1. **Search + filter together freezes stepping.** Search counts hits over every record with no
   `isHiddenRec` gate (`export.js:3111`), but a filtered-out record is never mounted
   (`setWindow`, `export.js:344/365/385`). `stepHit` calls `matRecord(hr.rec)`, gets null, and
   nothing moves — violating the rule the file states in capitals at `export.js:3217`:
   **"The rule now: EVERY press MOVES."**
2. **`Command` is a filter key that can never hit.** `buildToolMenu` emits it; `computeFilterHits`
   gates on `!isTurnKind(b)`, and `isTurnKind` includes `command`. Zero hits, always.
3. **A query silently replaces a filter (app shell).** `activeMatches` treats the text query as an
   override, so typing while a tool is ticked drops the filter's hits from the count and from ↑↓
   — while `applyFilters` still paints `.filter-hit` accents. Blue-accented heads the step control
   refuses to visit.
4. **The ⌘K palette ignores the sidebar's own filters.** It iterates raw `groupedSessions()`, not
   `visibleTree`, so it lists sessions the reader has hidden, and opening one shows a session the
   tree says is not there.
5. **`uiState.globalIndex` is dead state.** Set, clamped, painted `.active` — never advanced. The
   palette looks like a command palette and cannot be driven from the keyboard.
6. **`/` means two different things** despite one shared keymap entry: the session filter on
   classic, the transcript box on the app shell.
7. **A `raw` body part is invisible to search.** Rust emits `{"p":"raw"}` for preformatted runs
   lifted out of a user turn and both pages render it, but neither `ownTextParts` nor
   `recordTextSize` handles it. Pasted terminal output cannot be found.
8. **Three transcript-search semantics in the app shell**: the header box (scoped, cached,
   whole-word, 2-char floor), the ⌘K transcript tab (`plainText`, unscoped, no floor, no cache),
   and `applyFilters`'s scope dimming (which reads the DOM). Same word, three answers.

---

## Cross-session search: what it actually costs

The owner's constraint is explicit — a design needing a backend overhaul is not the direction. So
this was measured on this machine rather than estimated.

**There is no server-side search today.** Not one route on either binary: the monitor's table is
`api/{ui,sessions,ignore,send,consent}` plus the session service's `session`/`pull`/`records`/
`file`/`__reveal`. Every match in the product is JavaScript over records the client already holds.
So cross-session search is not an extension of anything. It is the first server-side search — and
that is the honest framing of its cost.

**The corpus, and the good news:**

| | |
|---|---|
| transcripts on disk | **1,033 files, 2.30 GiB** |
| size distribution | median 283 KB, p90 875 KB, **max 419 MB** — the 10 largest files hold 70% of all bytes |
| **warm full grep** | **0.14 s** (14 threads) · 0.35 s single-threaded |
| worst query shape measured | 0.24 s (case-insensitive literal) |
| cold bound | 0.32 s of I/O (2.47 GB at a measured 7.8 GB/s); corpus fits many times in RAM |
| growth | ~25 MB/day → ~9 GB in a year → still ~0.5 s warm |

**A full grep of every transcript on this machine costs less than a fifth of a second.** No index.
No schema. No cache to keep coherent. That is the finding that decides the design: the expensive
option was never necessary.

Three costs that are real and must be designed around:

- **Per-file subprocess is fatal**: 1,033 × ~60 ms of process startup ≈ **62 s** before any work.
  It has to be in-process, which the engine already supports — `LineSource` with the adapter's
  elision (`engine/elide.rs`) streams a transcript without materialising its giant lines.
- **87% of the files are sub-agent transcripts**, holding 41% of the bytes, and `--paths --all`
  returns 114 rows against 1,033 files. Coverage means `store_all()` **plus**
  `store_subagent_transcripts()`, or the answer is silently a third of the truth.
- **Hit lines are enormous**: on real queries, 6–13% of hits sit inside lines over 64 KB and
  account for **75%+ of the hit bytes**. Snippets must be clipped hard, at the source.

### The semantic trap, and the way through it

A raw JSONL grep matches the **JSON** — field names, ids, base64 — which is exactly what
`shared/search.js` promises it will not do. Shipping it as-is would give the reader a *second
search language* whose results they cannot predict from the first.

The way through is two-stage retrieval, which is also how every tool that does this works:

```
   typed once                                     ┌─────────────────────────────┐
   ───────────                                    │ 1. WHICH SESSIONS?          │
   "byte-identical"  ──▶  raw stream grep  ──────▶│    a ranked list of sessions │
                          (0.14 s, elided,        │    "12 sessions mention this"│
                           sub-agents included)   └──────────────┬──────────────┘
                                                                 │  reader picks one
                                                                 ▼
                                                  ┌─────────────────────────────┐
                                                  │ 2. WHERE IN IT?             │
                                                  │    the SAME in-session      │
                                                  │    search, same haystack,   │
                                                  │    same scopes, same steps  │
                                                  └─────────────────────────────┘
```

Stage 1 answers the reader's actual question — *which session was that in?* — and is allowed to be
approximate, because stage 2 is exact and is the one that puts a hit on screen. The elision the
engine already applies keeps base64 out of stage 1 for free. What stage 1 must never do is
**report a count as if it were the in-session count**: it says "12 sessions mention this", never
"47 hits".

---

## Question 1: one box or two?

**Two, and they are not the pair anyone assumed.**

The controls do not divide by *where the box sits*. They divide by **what the answer is**:

```
   "WHICH SESSION?"                       "WHERE IN THIS ONE?"
   the answer is a session                the answer is a position
   ┌──────────────────────────┐           ┌──────────────────────────┐
   │ (a) find by name/agent   │           │ (b) text, by kinds       │
   │ (NEW) find by content    │           │ (c) step one tool's calls│
   └──────────────────────────┘           └──────────────────────────┘
          the LOCATOR                             the READER
   opens with ⌘K, closes on pick          lives in the header, persists
```

The new requirement lands on the **left**, with the session finder — not as a third thing. You
already know which session you want when you use the right-hand box; the left-hand box exists
precisely for when you don't.

**Precedent, and it is overwhelming.** Every serious tool with a corpus and a document ships
exactly these two, and keeps them apart:

| | locator (which document) | reader (where in it) |
|---|---|---|
| VS Code | ⌘P / ⌘⇧F search across files | ⌘F find in file |
| IntelliJ / Xcode | Search Everywhere / project find | ⌘F |
| Chrome DevTools | ⌘P open file, ⌘⇧F all sources | ⌘F in panel |
| Slack | search messages | ⌘F in channel |
| Notion, Linear, Figma | ⌘K palette | in-page find |
| a browser | address bar | ⌘F |

Nobody merges them, and the reason is structural: **the two have incompatible lifecycles.** A
locator is *modal and transient* — it opens, takes a word, returns a thing, and closes. A reader's
find is *persistent and stateful* — it keeps a needle, a scope, a hit index, a highlight over the
document you are reading, and it must survive you scrolling around. Putting both in one input
means one of them loses the behaviour that makes it work.

The counter-examples prove the rule. Where one box does both — Spotlight, Alfred — there is no
document being read, so there is no persistent state to lose. And the tabbed palette this codebase
already has is the shape that fails: its Transcript tab had to carry an apology in the UI —
`"Transcript search covers the current session only"` — because a modal palette is the wrong home
for a stateful reader's find.

**So: keep two, and make them honest.** Today they are two boxes with four semantics. The
recommendation is two boxes with **one definition of a match**.

---

## Question 2: (b) and (c) — one language, one more term

They are not overlapping features to reconcile. They are two quarters of one control. The smallest
change that makes them one:

**Give the scope grammar a value term.** It has classes; it needs names.

```
   today          uatobrew:needle           classes only
   proposed       uatobrew:needle           unchanged, still valid
                  tool:Bash needle          one tool, by display name
                  tool:mcp__foo__* needle   a prefix, which the filter already supports
                                            (`want.toolPre`, export.js:1714)
```

That single term subsumes the tool filter: **case (c) becomes `tool:Bash` with an empty needle** —
"every Bash call, stepped" — which is precisely what you said the filter was for. And it composes,
which neither control can do today: `tool:Bash timeout` is "occurrences of *timeout* in Bash
calls", a question no box can currently ask.

**One result model, and it is the app shell's.** #133 already decided this and the comment states
why:

> Hiding everything else takes the context away at exactly the moment the reader found what they
> were looking for.

So the filter stops being a cut. That deletes bug 1 (search and filter can no longer disagree
about what is mounted, because nothing is unmounted), it deletes the sparse-window code path the
classic page carries for filtering, and it removes the thing that makes a filtered window "a
different beast for the virtual window" — which `#140`'s port would otherwise have to reproduce.

**The menu stays.** A grammar you must know is not a UI. The funnel keeps its checkboxes and gains
a tool list; ticking a tool writes `tool:Bash ` into the box, exactly as ticking `u` writes `u:`
today. The box remains the single source of truth, so the two can never drift.

---

## The recommendation, in order

Each step is separately shippable and separately valuable. **Steps 1–3 need no backend at all.**

1. **One definition of a match.** Delete the ⌘K palette's `plainText` matcher and the scope-dimming
   DOM read; both call `shared/search.js`. Fix the `raw` gap while there (and note that
   `ownTextParts` and `recordTextSize` are parallel field lists — adding a field is two edits, and
   this is exactly the shape of the bug).
2. **`tool:` in the scope grammar**, in `shared/search.js` so both pages get it at once. The tool
   filter becomes a menu that writes into the box.
3. **The filter stops hiding.** Classic adopts the app shell's mark-and-step. Bugs 1, 2 and 3 go
   with it; the classic page's sparse-filter path can be deleted rather than ported (#140 step 3
   changes shape — worth deciding together).
4. **The locator learns content.** One new route, `api/search`, in the shared `routes.rs` table so
   both monitors get it: `LineSource` + elision over `store_all()` **and**
   `store_subagent_transcripts()`, a thread pool over files, hard-clipped snippets, and a result
   that names *sessions*, not hits. Stage 2 is the existing in-session search, unchanged.
5. **Fix the palette as a palette** — honour `visibleTree`, make `globalIndex` live, and give a
   project row a "reveal in the tree" action instead of opening an arbitrary session.

What this deliberately does **not** do: no index, no schema, no cache to keep coherent, no query
language beyond one new term, and no new definition of a match. The backend grows by one route
whose body is a loop over a reader that already exists.

---

## Open questions

| # | Question | Why it needs you |
|---|---|---|
| A | Is **`tool:` by display name** right, or should it be the raw name? Display name folds `Edit`+`MultiEdit` into `Update` — convenient, and lossy. | It is the user-facing vocabulary either way; you use it daily and I do not. |
| B | Should stage 1 search **sub-agent transcripts**? They are 87% of the files and 41% of the bytes. Including them finds more; it also means "which session?" can answer with a child you never opened directly. | It changes what the result list means. |
| C | The 65 MB of **spilled tool outputs** under `tool-results/` are content the agent saw but are not in the JSONL. In scope, or out? | Out is defensible and much simpler; in is a second corpus. |
| D | Does the locator search **hidden** sessions? The tree hides them deliberately; a content search that skips them cannot answer "which session was that in" when the answer is a hidden one. | Two defensible answers, and the current palette gets it wrong by accident rather than by choice. |
| E | Is `#140`'s sparse-filter step still wanted if step 3 deletes the sparse filter? | It would remove work from a task already queued. |
