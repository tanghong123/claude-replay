# CC activity coalescing — the empirically derived rule (#57)

Claude Code renders **one summary line per span of agent work between visible
outputs**, not one line per assistant message. This doc records the rule as
derived by black-box probing of Claude Code v2.1.220 (2026-07-29): four
synthetic sessions were resumed in a real `claude` TUI inside a private
`tmux -L` server and its rendering captured via `capture-pane`, then validated
against CC's rendering of a real span of the claude-replay dev session
(`094539f2`, the span the user's #57 comparison paste came from — CC's
"Thought for 22s, pushed to main, ran 6 shell commands" is reproduced exactly
by this rule: 7 bash commands, one a `git add/commit -q/push` compound whose
push output names `main`, split from the following "Thought for 45s, ran 3
shell commands" by two invisible-but-breaking `TaskUpdate` calls).

## Span structure

A **span** accumulates consecutive thinking blocks + *activity* tool calls.
It is flushed as ONE summary line when a **breaker** renders:

- assistant text output
- a user turn / slash command / queue marker
- an expanded tool: Edit/Write/NotebookEdit (`⏺ Update(file)` + diff),
  WebFetch (`⏺ Fetch(url)`), Task/Agent spawns, Skill, MCP tools, and any
  other non-activity tool
- task-bookkeeping tools — TaskCreate/TaskUpdate/TaskGet/TaskList/TodoWrite —
  **break the span but render nothing at all** in CC (verified twice: the
  synthetic probe and the real-session split above)

**Transparent** (does NOT break, invisible to CC): attachment events
(`edited_text_file` etc.) — CC's 22s span carries across one. replay keeps
rendering these blocks (fidelity), placing them before the span's summary.

replay deviation (deliberate): task-bookkeeping tools stay visible as their
small folded ToolUse blocks — hiding data outright is against the viewer's
purpose — but they break spans exactly like CC, so the summary-line structure
matches CC 1:1.

## The summary line

`Thought for <dur>, committed <hashes>, pushed to <branches>, searched for N
patterns, read N files, listed N directories, ran N shell commands`

- Clause order is FIXED as above; clauses with zero count are omitted; the
  first clause is capitalized. No thought → activity clauses only
  ("Pushed to main", "Listed 1 directory"). Tools but no thinking, or a single
  activity tool — still folded to the summary (CC never leaves an activity
  tool expanded, even alone).
- **Duration** = SUM over the span's thinking blocks of
  (thinking.ts − previous event's ts) — *previous event of any kind*,
  including assistant text (verified: a thinking 4s after its own turn's text
  but 9s after the last tool result shows "Thought for 4s"). NOT wall-clock
  (a 3-minute span with 10s+7s+3s thinking gaps shows "Thought for 20s").
  Format: `Xs`, or `Xm Ys` at ≥60s ("1m 15s").

## Activity classification

- `Grep`/`Glob` tools → *searched* (occurrences).
- `Read`/`NotebookRead` tools → *read*, counted by **unique file path**
  (Read a, Read a, Read b → "read 2 files"; a bash `cat` of the same file
  dedupes against a `Read` of it).
- Bash commands classify semantically ONLY when they are a single simple
  command; ANY compound — newlines, pipes, `;`, `&` — counts as a plain shell
  command (validated on the dev session: CC tallied its `cd X\ngrep … | head`
  compounds under "ran 6 shell commands"/"ran 3 shell commands", never as
  searches/reads). For single commands, by first word:
  - search words (grep/rg/fd/find/ag) → *searched*
  - read words (cat/head/tail/less/more/bat) → *read* (dedup by file arg)
  - `ls` → *listed N directories* (occurrences — NOT deduped: `ls /tmp`
    twice → "listed 2 directories")
  - anything else → *ran N shell commands*
  Git phrases are the exception — they apply to compounds too, keyed on the
  OUTPUT:
  - `git commit` whose output parses `[branch hash]` → **committed hash**
    (a `-q` commit has no parseable output → no clause)
  - `git push` whose output parses `… -> branch` → **pushed to branch** —
    the branch comes from the OUTPUT (not the command args, not the event's
    `gitBranch`); `[new tag]` lines are skipped (a commit+push-with-tag
    compound reads "pushed to main" alone); a failed push (no `->`) falls
    back to *ran*; multiple pushes join: "Pushed to dev, main"
  - phrased commands are NOT double-counted under *ran*
- CC does NOT name shell programs — replay drops its former
  "(grep, python3)" parenthetical extension for parity.

## Probes (evidence)

S1: sum-vs-wallclock, Edit breaks+expands, `ls` → listed, thought-first order.
S2: bash grep/cat classes, read dedup, TaskUpdate invisible break,
    "Pushed to main", Write breaks, lone-activity folds, no program names.
S3: "1m 15s" format, read dedup by path, listed not deduped, push branch from
    output, "Committed abc", Glob → searched, WebFetch expands+breaks,
    TaskCreate/TodoWrite invisible break.
S4: full clause order, compound commit+push both clauses, MCP breaks,
    failed push → ran, double push merges.
Real-session validation: the #57 paste's three CC lines reproduce exactly.
