# claude-replay — design & todo

An interactive, **read-only** Claude Code session viewer: *as if you ran
`claude --resume`, but you can't type commands.* Successor to the bash
`claude-peek` pager.

Status: **built — P0–P8 complete** (see the phased todo below). This file remains
the spec/scope of record.

> Historical note: this crate was extracted from the `claude-toolbox` repo (the
> `justdoit/` module, formerly `keep/`, where it was `claude-peek-v2`). References
> below to `justdoit/peek-v2/`, `justdoit/install.sh`, and `claude-jdi` describe
> that sibling repo — the installer/supervisor integration lives there, not here.

## Decisions (locked)

- **Language / UI:** Rust + `ratatui` + `crossterm`, **borrowing from
  `claude-code-scrollback`** (MIT, Rust+ratatui, already ~5/6 of our features).
  Reuse its proven bits (byte-offset incremental tail, O(1) pre-rendered line
  cache, fold model) under MIT attribution; don't reinvent.
- **Shipping shape:** new command **`claude-replay`** for now, installed
  alongside the bash `claude-peek`. **Eventually replaces** `claude-peek` (at
  which point the bash tool's pipe/non-TTY behavior survives as a `--plain` mode
  so `claude-jdi log` and scripts keep working). Until then `claude-peek` is
  unchanged.
- **v1 scope:** the six core features below **plus** session picker, in-transcript
  search, word-level Edit diffs, and a metrics line.

## Core feature requirements (the six)

1. **Live tail.** Follow a running session; new events appear without restart.
   Borrow scrollback's byte-offset incremental `TailReader` (+ `notify` watch,
   poll fallback; handle file reset/rotation; skip malformed lines).
   *Accept:* `--live`/`-f` follows the latest active session in cwd and appends
   new turns as they're written.
2. **Markdown rendering.** `pulldown-cmark` → ratatui lines, **with syntax
   highlighting** for fenced code (add `syntect` — scrollback lacks this).
   Width-aware wrap, CJK/emoji via `unicode-width`. *Accept:* headings, lists,
   bold/italic, inline code, fenced code blocks render styled; code blocks are
   highlighted.
3. **Colors/styles matching Claude Code.** A theme that mirrors Claude Code's
   palette/affordances (`●` assistant, `❯` user, `⏺` tool, `⎿` result, `✻`
   thinking), not a generic scheme. *Accept:* side-by-side with `claude --resume`
   reads as visually consistent.
4. **Mouse scrolling.** Wheel scrolls the transcript (and the picker/search
   lists). Enable via `crossterm` mouse capture — the gap scrollback never filled.
   *Accept:* wheel up/down scrolls smoothly; selection/scroll respond to clicks.
5. **Fold/expand sections.** Collapse/expand tool calls, tool results, thinking,
   and long blocks — by **hotkey** (`t` toggle under cursor, `T` collapse/expand
   all of a kind) **and mouse** (click the block header). Collapsed blocks show a
   one-line placeholder. *Accept:* a long tool-result folds to one line and
   re-expands by key or click.
6. **"N new messages" badge while scrolled back.** When you've scrolled up
   (follow paused) and new events arrive, show a bottom bar like
   `▼ 3 new messages — G to jump`; clear it on jump-to-bottom / resume-follow.
   **None of the four studied tools do this** — our differentiator. *Accept:*
   scroll up during a live run, see the count grow, press `G` to jump and clear.

## v1 add-ons (chosen)

7. **Session picker.** With no id/path, list the cwd's sessions ranked by
   directory affinity (recent first), fuzzy-filter with `nucleo`, Enter to open.
   Borrow scrollback's picker. *Accept:* `claude-replay` with no args opens a
   picker; `claude-replay <id|path>` / `--latest` skip it.
8. **In-transcript search.** `/` incremental search, highlight matches, `n`/`N`
   to jump. *Accept:* searching narrows/highlights and navigates matches.
9. **Word-level Edit diffs.** Render Edit/MultiEdit as red/green with the changed
   *words* highlighted (borrow `claude-code-trace`'s approach) rather than
   whole-line +/-. *Accept:* an Edit shows intra-line word changes.
10. **Metrics line.** Footer/per-turn metrics: tokens, USD cost, duration, short
    model name (borrow `claude-code-trace`'s formatters). *Accept:* footer shows
    session totals; optionally per-turn.

## Notable edges

- **Real thinking summaries.** `claude-code-trace` assumes thinking isn't in the
  logs (shows a placeholder) — outdated. With `showThinkingSummaries` on (the
  `justdoit` installer enables it), `.thinking` carries summaries; render them as `✻`
  blocks (foldable). This beats all four studied tools.
- Pin schema expectations to a Claude Code version (scrollback pins ~v2.1.x) and
  snapshot-test against real transcript fixtures; skip/҂log unknown event types.

## Live-tail turn grouping (current behavior)

How a thinking block and the activity tools it processed collapse into one
`✻ Ran N …, thought for Xs` line — and why the live path differs from a full
re-parse.

**Transcript event order (the ground truth).** Claude Code writes a `thinking`
content block to the JSONL **only once it is complete** — as a field inside an
assistant message (`model.rs` `parse`, the `"thinking"` arm). There is **no**
"thinking started" or "thinking ended" marker; the block is atomic on disk. What
lands incrementally during a run is the **tool calls** — each `Bash`/`Read`/… is
its own assistant message, written as the tool runs. The thinking block that
"owns" them is the reasoning step that processed their *results*, so it appears in
the stream **after** those tools. Accordingly `model::group_turns` folds a
`Thinking` together with the contiguous run of **activity** tools
(`is_activity_tool`: Bash/Read/NotebookRead/Grep/Glob/LS) that **immediately
precede** it; Edit/Write and other durable-output tools stay expanded.

**Full parse (quit+restart, `--dump`).** `parse` sees the whole file and runs
`group_turns` once at the end, so every thinking block is correctly grouped and
carries a duration (`thinking_ts − trigger_ts`, floored — `trigger_ts` is the last
user/tool-result timestamp).

**Live tail.** `tail.rs::TailReader::poll` returns only the *new* complete lines
since the last poll; `app.rs` parses just those and calls `View::ingest`. So there
is no single "begin/finish thinking" UI phase — the transcript gives us nothing to
render there. The observable sequence is two states:
1. Tools stream in **expanded and linear** (`⏺ Bash(ls)`, `⏺ Read(x.rs)`, …),
   growing — no thinking line yet, because none has landed.
2. The thinking block lands → on that poll, `View::ingest`'s **seam-merge**
   retroactively steals the trailing activity tools off the already-ingested block
   list, folds them into the new `Thinking`, truncates the positional `body_cache`,
   and the whole run collapses to the one-line summary (`render.rs` `turn_summary`).

It flips **directly** from "expanded tools" to "one collapsed summary" — there is
no transient "finishing…" line. (Before the seam-merge fix, `group_turns` ran per
poll-batch and never absorbed tools from an earlier poll, so grouping only appeared
after quit+restart.)

**Known residual.** A live-collapsed line reads `…, thought` **without** the
`for Xs` (and a tool-less thinking falls back to `Thought (N lines)`). Duration is
`thinking_ts − trigger_ts`, but `trigger_ts` is only known *within* one parse batch
(`model.rs`); when the thinking block arrives in a later poll than its triggering
user/tool-result event, the duration comes out `None`. Quit+restart (a full
re-parse) recovers it. See the backlog item below.

## Borrow map (all MIT)

- **claude-code-scrollback** → tail (byte-offset incremental), line-cache scroll,
  fold model, dir-affinity picker, turn/checkpoint navigation, malformed-line
  handling. *Closest base; preserve its MIT notice for any copied code.*
- **claude-code-trace** → word-level Edit diffs, subagent drill-down (future),
  metric formatters (tokens/cost/duration, model short-names), MCP tool-name
  humanizing.
- **cass** → (future) cross-session search: BM25 (+optional embeddings), RRF +
  recency ranking — for a later `claude-peek search`, not v1.
- **session-manager-tui** → overlaps `claude-jdi`/`takeover`; little to borrow.

## Non-goals (v1)

- Cross-session / cross-agent search (that's `cass`'s lane; maybe a later subcmd).
- Writing/continuing the session (read-only by definition — to act, use
  `claude-jdi takeover` → `claude --resume`).
- Web/desktop frontends. Terminal only.

## Visual-parity harness (sibling repo — not here)

The ground-truth fixtures and tooling for "does claude-replay render like the real
Claude Code?" live in the **private** sibling repo **`claude-replay-eval`** — keep it
out of this public repo (it contains real session transcripts). What's there:
- `golden/cc.scroll.{txt,ansi}` (+ per-frame `cc.NNN.*`) — Claude Code's own render of a
  session, captured at a fixed geometry; the comparison ground truth.
- `capture-golden.sh` — mint a fresh golden from a session id by driving real
  `claude --resume` read-only in headless `tmux` and stitching the screens.
- `capture-peek.sh` — drive *this* viewer over the same transcript and snapshot frames.
- `stitch-frames.py` — concatenate frames (de-dup overlap, strip chrome) into a scroll.
- `compare-scroll.py` — diff cc vs peek (text + `--ansi` colour); minimise "CC unmatched".
- `COMPARE-CC-vs-peek-TASK.md` — the end-to-end driving/comparison procedure and caveats.

### Calibration run (golden `claude-replay-20260630-173x47`, width 173)

Iterated `--dump … --width 173` vs `cc.scroll.{txt,ansi}`. CC-lines-unmatched went
**43.4% → 9.5%**. Fixes shipped: route `--dump` through the View pipeline (wrap +
`fill_bg` + diff inset); number diff **deletions** with the old-side line number;
**hanging indent** on wrapped continuations; code blocks at the body indent with no
blank-before; `--dump` folds by default (`--full` expands); coalesce same-style spans
in `.ansi`; heading-opens-block no longer steals the `⏺` marker; **turn grouping** —
a thinking block absorbs its preceding activity tools and renders as CC's
`Thought for Xs, <activities>` (duration floored from transcript timestamps).

The residual diff is **not** decision-free rendering:
- **Tables (~49 lines):** column widths intentionally differ (our fair-share algorithm);
  cell font/colour styling already matches CC.
- **CC live-UI chrome (~31 lines):** `✻ Worked for …`, `※ recap: …`, `⏺ Background
  command … completed` — ephemeral CC UI, not transcript content; correctly absent here.
- **Bash command semantics (~a few lines):** CC categorizes a `Bash` running `ls` as
  `listed 1 directory`; we count all Bash as `ran N shell commands`. Matching needs
  parsing the shell command — deferred (best-effort scope).

## Phased todo (queue)

- [x] **P0 scaffold.** `justdoit/peek-v2/` cargo crate; deps: ratatui, crossterm
  (mouse), pulldown-cmark, syntect, nucleo, notify, serde_json, clap,
  unicode-width. Decide fork-vs-vendor of scrollback modules; record attribution.
- [x] **P1 static viewer parity.** Parse JSONL → roles/blocks; render
  user/assistant/tool/result/thinking; markdown+syntect; Claude-matched theme;
  keyboard **and mouse** scroll; open `<id|path>` / `--latest`. (features 2,3,4)
- [x] **P2 live tail.** Incremental byte-offset tail + watch; follow/pause; `-f`.
  (feature 1)
- [x] **P3 new-message badge.** Scroll-position vs tail tracking; bottom bar +
  `G` jump. (feature 6)
- [x] **P4 fold/expand.** Collapsible tool/result/thinking/long blocks; `t`/`T` +
  mouse-click headers. (feature 5)
- [x] **P5 session picker.** Dir-affinity ranked, nucleo fuzzy. (feature 7)
- [x] **P6 in-transcript search.** `/`, highlight, `n`/`N`. (feature 8)
- [x] **P7 diffs + metrics.** Word-level Edit diffs; metrics footer. (features 9,10)
- [x] **P8 integration.** `justdoit/install.sh` builds+installs `claude-replay`
  (cargo build, or prebuilt); document; plan the eventual `claude-peek` swap with
  a `--plain` fallback so `claude-jdi log`/pipes keep working.

## Backlog (queued post-v1 improvements)

> **Reproducing transcript** for the table, multi-line-args, and skill-folding items
> below: any session that contains a wide markdown table, a multi-line `/loop`
> slash-command invocation, and an injected skill-instruction body. Use one to
> confirm the JSONL markers before building. (Private captures live in the sibling
> `claude-replay-eval` repo, kept out of this tree.)


- [x] **Fair-share table column widths (finalized algorithm).** ✅ shipped `d41e6ad`. `render_table` in
  `markdown.rs` currently shrinks the *widest* column by 1 repeatedly, which can
  over-shrink one column while others stay wide. Replace with max-min fair sharing, and
  fix the budget:
  1. **Remove the `MAX_TABLE_WIDTH` (100) cap** — wide terminals get wide tables.
  2. **Margins:** reserve 2 blank columns on the left (already supplied by the
     assistant body indent) + 3 on the right = 5 total, so `budget = width − 5` (was
     `width − 2×7`). Keep a fallback budget for the pre-layout `width == 0` case.
  3. Compute each column's max content width (to render its text without wrapping).
  4. **If `sum(max_widths) ≤ budget`, return the natural widths unchanged** (no
     expand-to-fill — a narrow table stays narrow).
  5. Otherwise run fair sharing: give each unfixed column a quota of
     `remaining_budget / remaining_cols`; any column whose max width ≤ quota is fixed at
     its max width and removed from the pool (freeing budget for the rest); repeat until
     only over-quota columns remain — those split the leftover budget evenly and wrap.
  Add unit tests for the allocator (under-budget → unchanged; over-budget → narrow cols
  keep natural, wide cols share) and confirm the budget/margin math.

- [x] **Distinct background for expanded foldable blocks.** ✅ shipped `070bde3`. When a foldable block is
  expanded, fill its *whole* block (all physical lines, edge-to-edge per `fill_bg`)
  with a distinct background so it reads as one delimited region. Today only expanded
  shell/read foldables get this (`theme::shell_expanded_bg()` via `view.rs`); generalize
  it so every foldable type (generic `tool`/`command` calls, `tool_result`, thinking,
  etc.) gets a block background when expanded. Reuse/extend the existing background-tier
  ladder in `theme.rs` (user > shell/read > thinking) rather than inventing ad-hoc
  colors, and keep the collapsed one-line summary visually distinct from the expanded
  fill. Add a `TestBackend` test asserting an expanded foldable's interior rows carry
  the block bg and a non-foldable block's rows don't.

- [ ] **Group skill loading into one foldable block, collapsed by default.** When a
  skill is invoked, Claude Code reads the skill's files (`SKILL.md` + bundled files)
  as a burst of `Read`/tool calls. Detect that skill-load sequence and nest the actual
  skill-file reads *inside* a single "skill" foldable block (the whole load becomes one
  collapsible region) instead of surfacing each file read as its own top-level block.
  Add a `"skill"` fold key (`model::fold_key` / `tool_fold_key`) and include it in the
  `FoldPolicy` default-folded set so the skill-loading block starts **collapsed**
  (like thinking/reads). Collapsed summary should name the skill; expanding reveals the
  nested file reads. *To resolve first:* how a skill invocation and its file reads are
  represented in the JSONL (Skill tool_use marker? a system message? a recognizable
  Read burst?) — confirm against a real transcript before implementing the grouping.

- [x] **Preserve line breaks in multi-line slash-command args.** ✅ shipped `3740c77`. `command_header`
  (`render.rs`) builds `format!("{name} {args}")` as a single ratatui `Line`, so a
  slash command with a multi-line argument (e.g. a long `/loop` prompt) collapses to one
  run — embedded `\n`s are lost and text jams together ("…capture.WORKING DIR:"). Split
  the args on newlines and emit one `Line` per source line: `❯ /cmd <first line>` then
  continuation lines aligned under the caret, all on the `user_bg` block. The expanded
  render path (`render.rs:422`) should show the full multi-line body; the collapsed
  summary / `render_collapsed` paths (~`:532`, `:592`) stay one line — first arg line
  plus an ellipsis when there's more. Update the command-block tests to cover a
  multi-line arg.

- [x] **Fold the injected skill/command instruction body (collapsed by default).** ✅ shipped `733d957` (also named the Skill tool target). When
  a skill/slash-command loads, Claude Code injects the skill's instruction markdown (e.g.
  the whole `# /loop — schedule…` body) as a message. The viewer currently models it as a
  plain `Block::UserText`, which is **not** in the foldable set (`render.rs` `is_foldable`
  folds only ToolUse/ToolResult/Thinking/Command), so it always renders fully expanded and
  buries the real transcript. Detect these injected instruction messages, model them as a
  dedicated foldable block (or route them through the `Command`/`"skill"` path), and add
  the key to `FoldPolicy`'s default-folded set so they start **collapsed** — header names
  the command/skill, expansion shows the body. Must NOT fold genuine user prose: key off
  the JSONL wrapper that marks injected instruction content (confirm the exact marker —
  likely a `<command-message>`/skill-content tag — against a real transcript). Closely
  related to the "group skill loading" item above and the multi-line-args item; consider
  implementing together.

- [x] **Fold background-execution notifications into a one-line summary.** ✅ shipped `4bb11e7`. Background
  command / task completions arrive as user messages wrapping a `<task-notification>`
  …`</task-notification>` block (with `<task-id>`, `<tool-use-id>`, `<output-file>`,
  `<status>`, `<summary>` children). The viewer currently models these as plain
  `Block::UserText`, so the whole raw XML renders inside a `❯` user block. Claude Code
  instead shows a single clean line — `⏺ Background command "Build release and report
  binary" completed (exit code 0)` — sourced from the `<summary>` child. Detect the
  `<task-notification>` wrapper in `model::push_user_string` (alongside the existing
  `<command-name>` / `<local-command-stdout>` / caveat handling), extract `<summary>`
  (and `<status>`), and render it as a tool-style line (`⏺ <summary>`) rather than a
  user turn — folded/compact by default, with the raw XML dropped. Confirm the exact
  child tags against a real transcript; reuse `tag_inner`. (The `<system-reminder>`
  background-task event variant should fold the same way.)

- [ ] **Full-document dump to files at a chosen width, in both txt and ansi.** Today
  `--dump` (`app::dump`) renders the transcript flat (no folding) at a hard-coded
  `DUMP_WIDTH = 100`, **plain text only**, to **stdout**. Extend it to render the whole
  transcript as one infinitely long document laid out to a chosen width and write BOTH a
  `.txt` (spans flattened — current behavior) and a `.ansi` (each line's spans re-emitted
  as SGR escapes from its ratatui `Style`: fg/bg 256-colour indices + bold/italic/dim,
  reset per line). Mirrors the external `capture-peek.sh` → `pk.scroll.{txt,ansi}`.
  - **CLI (decided):** make `--dump` take an optional value (clap `num_args(0..=1)`,
    `Option<Option<String>>`); update `main.rs`'s `!args.dump` gate accordingly.
    - `--dump <stem>` → write `<stem>.txt` + `<stem>.ansi`.
    - `--dump` (no value) → write files using a **deduced default stem** (below).
    - `--dump -` → keep the current **stdout** plain-text behaviour (so the documented
      quick-check survives); update `CLAUDE.md`'s `--dump` example to `--dump -`.
  - **Default stem:** `<basename>-<pathhash>-<sessionid>-<width>`, e.g. for this repo at
    width 140 → `claude-replay-<6hexhash>-<first6ofsessionid>-140` (so `.txt`/`.ansi`).
    - `basename` = basename of the session's **project cwd**; `pathhash` = first 6 hex of
      a hash of the full project cwd path (disambiguates same-named dirs); `sessionid` =
      first 6 chars of the session id; `width` = the render width actually used.
    - Source `cwd` and `sessionId` from the transcript JSONL (reliable — same approach as
      `capture-golden.sh`), not by decoding the project dir name (ambiguous for paths
      with `-`). Files are written in the current working directory.
  - **Width:** default to the real terminal width via `crossterm::terminal::size()`,
    fall back to `DUMP_WIDTH` (100) when there's no TTY; `--width <N>` overrides. The
    width that's used goes into the stem.
  - **Test:** assert the `.txt` has no escape codes, the `.ansi` strips back to the same
    text, and the deduced stem matches `<basename>-<6hex>-<6id>-<width>`.

- [x] **Match CC table font styling.** ✅ shipped `74d81c0` — default border colour (no
  gray), non-bold header cells. (Table *column widths* stay on our fair-share algorithm —
  intentionally not matched to CC.)

- [ ] **Carry `trigger_ts` across poll batches so live-tailed thinking shows its
  duration.** See "Live-tail turn grouping" above: a thinking block ingested in a
  later poll than its triggering user/tool-result event renders `…, thought` (no
  `for Xs`) because `parse` computes duration only within a single batch. Persist
  the last-seen trigger timestamp on `View` (or thread it through `ingest`): when a
  batch's opening `Thinking` has `duration_secs == None`, recompute it from the
  stored `trigger_ts` and the thinking message's own timestamp. Needs the thinking
  block's timestamp to survive into `ingest` — either stash it on `Block::Thinking`
  or pass it alongside the batch. Add a `TestBackend`/`ingest` test: poll 1 = a
  user turn + activity tools, poll 2 = a lone `Thinking`; assert the collapsed
  summary reads `…, thought for Xs`, matching a full re-parse of the same lines.
  *(Deferred once as "minimal benefit"; captured here so the fix is scoped.)*

### Cleanup tasks

- [x] **Sync the backlog checkboxes with reality.** ✅ done — the shipped items above now
  read `- [x]` with their commit; group-skill-loading and the dump-to-files item stay open.
- [x] **Cross-reference the golden-capture / parity tooling.** ✅ done — see the
  "Visual-parity harness" section above and the README note. The visual-parity harness
  and golden fixtures live in the sibling **`claude-replay-eval`** repo (private):
  `capture-peek.sh` (drive this viewer), `capture-golden.sh` (drive real `claude --resume`
  to mint a golden from a session id), `stitch-frames.py`, `compare-scroll.py`,
  `golden/cc.scroll.{txt,ansi}`, and `COMPARE-CC-vs-peek-TASK.md`. Add a short pointer to
  it here (and/or in `README`/`ATTRIBUTION`) so the parity workflow is discoverable — it
  is NOT in this repo and must stay out of the public one (it contains real session text).

## Open questions (revisit later)

- Build/distribution in `install.sh`: require `cargo` to build from source, or
  ship prebuilt binaries? (Affects the fresh-Mac bootstrap path.)
- Exact `claude-jdi log` wiring once v2 lands (default to v2 TUI on a TTY, fall
  back to `claude-peek --plain` when piped?).
