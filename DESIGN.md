# claude-replay — design & todo

An interactive, **read-only** AI-agent session viewer (Claude Code + Codex): *as if you ran
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
- **Cache and locking (settled 2026-08-08).** One cache implementation, one very simple
  locking model — no second, bespoke "private" cache anywhere.
  - **Where.** The viewer's durable entries are `~/.cache/claude-replay/sessions/<frontend>/<session-id>`;
    the monitor's are its own root `~/.cache/claude-monitor/` (R5 — deliberately not the
    viewer's). Both roots are overridable through the API and through
    `$CLAUDE_REPLAY_CACHE` / `$CLAUDE_MONITOR_CACHE`, which is how tests get an isolated
    root instead of contending with a running instance.
  - **Granularity.** The viewer locks per `<session, frontend>`; the monitor's root is a
    single entity under one lock. No pid in any durable path.
  - **`--no-cache` does not mean "no cache".** It means *don't use the shared root*: the
    run builds a throw-away cache with the same implementation at a **discoverable**
    location, so it can be found and swept later. An opaque per-pid temp path is not
    discoverable — that is how 8 GB and then 8,014 stray directories accumulated unseen.
  - **Denied ⇒ redirect, never a second tail.** A process that cannot take the lock does
    not fall back to its own copy. It redirects to the holder, whose URL comes from the
    lock's note: `claude-replay --html` serving one session redirects and quits; a picker
    stays on the picker (only the sessions it opens redirect); `claude-monitor` redirects
    and quits.
  - **The redirect rides the pull reply, not an HTTP 302.** A page that is already open
    learns it is in the wrong place through a `{"t":"redirect","url":…}` record it acts on
    by navigating. A 302 would be worse than useless: `fetch` follows one transparently, so
    the loop would keep pulling the new server with a cursor minted against the old one's
    record stream, and two folds of one transcript are not guaranteed to agree. A denial
    with no note — the holder took the lock but has not bound yet — has no target, so it
    answers `{"t":"error"}` naming the pid. Both reach the page: a viewer that cannot serve
    a session says so.

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

## Live-tail turn grouping (historical — pre-M16)

> **Note (post-refactor):** this section predates the engine refactor. Since M16 the live
> path folds through `FollowParser`/`Replayer` (which produce a fully-regrouped cumulative
> snapshot each poll), so the view no longer re-groups — the "live path differs from a full
> re-parse" concern below is resolved. Module references are also pre-split: the Claude
> tokenizer/`parse`/`group_turns`/`push_user_string` cited here now live privately in
> `claude_model.rs`; `model.rs` is the agent-neutral engine. Kept for the rationale; the
> current design of record is `design/parser-engine.md`.

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
  `Esc` in the viewer returns to the picker to switch sessions (when launched from
  it, i.e. more than one session); `q` always quits. Picker ↔ viewer run in one
  terminal session (`app::run_interactive`), reusing the `Picker` so its query and
  selection survive a round trip. On a `--latest` launch (no list shown), `s`
  opens the picker as an overlay over the current session — `Enter` switches
  (`Outcome::Switch`, re-viewed by `run_view_loop`), `Esc` closes it in place with
  no reload. The `s` key and the `Esc`-back line are gated by per-launch flags
  (`View::set_can_open_picker` / `set_can_go_back`), reflected in the `?` help.
- [x] **P6 in-transcript search.** `/`, highlight, `n`/`N`. (feature 8)
- [x] **P7 diffs + metrics.** Word-level Edit diffs; metrics footer. (features 9,10)
- [x] **P8 integration.** `justdoit/install.sh` builds+installs `claude-replay`
  (cargo build, or prebuilt); document; plan the eventual `claude-peek` swap with
  a `--plain` fallback so `claude-jdi log`/pipes keep working.

## Backlog (queued post-v1 improvements)

- [ ] **Generalize the HTML "Tools ▾" filter to a message-type filter.** The HTML export's
  top filter dropdown (`buildToolMenu`/`setFilter` in `html/export.js`, fed by the
  `data-tool` attribute the emitter sets on tool folds) filters only by *tool use* today.
  Generalize it to filter by **message/block type** so non-tool kinds — notably **Agent**
  (spawn + completion), and plausibly thinking/attachment/command — also appear as
  selectable filters. Likely: emit a `data-kind` (already present) or a broader
  `data-type` the menu enumerates, and rename the menu "Filter by type". Keep tool-name
  granularity as a sub-case. Applies to the served/live page and the bundle shell.

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

- [x] **Group skill loading into one foldable block, collapsed by default.** ✅ shipped.
  *Premise correction (investigated across 28 transcripts / 28 `Skill` invocations,
  2026-07-25):* skill loading does NOT produce a burst of skill-file `Read` calls —
  **zero** invocations were followed by one. Current Claude Code delivers the skill's
  instruction body inline: a `Skill` tool_use, then its `tool_result`, then an injected
  user text block starting `"Base directory for this skill: …"`. So there were never
  loose file reads to group — but the injected body WAS rendering as a separate loose
  result block beside the `Skill` call. The fix nests it: `attach_skill_body` appends
  that body into the preceding `Skill` tool_use's output (`model.rs`), so a skill load is
  ONE collapsible unit named by the skill. Added a `"skill"` fold key (`tool_fold_key`,
  `FOLD_KEYS`, `--fold`/`--unfold`) in the default-folded set so it starts collapsed;
  the HTML `skill` keyline (`--kw`) already existed. Orphan bodies (no preceding Skill)
  still fold as their own result.

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

- [x] **Full-document dump to files at a chosen width, in both txt and ansi.** ✅ shipped
  (`--dump [stem]` writes `<stem>.txt` + `<stem>.ansi`, `--dump -` → stdout, `--width N`;
  routed through the View pipeline in `fb2f2d1`, `.ansi` span coalescing in `c699bdb`,
  default fold in `d89cdcc`). Original design note below.
  Today
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

- [x] **Surface transcript attachments (file names + download).** ✅ shipped `af8ee72`
  (file/plan/edited/compact) + `9433787` (base64 images). The scoping/decisions below are
  kept as the design record. Delivered: the four file types + images surface as
  `Block::Attachment`; TUI = clickable name, `[`/`]`+Enter or click → download (embedded,
  sync save to ~/Downloads, never overwriting) / reveal (path-only); served `--html` =
  Blob/`data:`-URI download + `/__reveal`, inline image render; `--dump`/`--dump-html` =
  names only.
  Transcripts embed content (files, plans, pasted/read images) that the viewer drops
  today. Surface it — but decide **download vs. reveal-in-Finder vs. inline** by one rule.

  **Guiding principle.** Offer a **download** ONLY for content that is (a) *embedded in
  the transcript* AND (b) *not already shown inline* in the TUI/HTML. If the content is
  merely **referenced by a path** (not embedded), don't download — **reveal it in Finder**
  (via the existing `/__reveal` / `app::reveal_in_file_manager`). If we already decode and
  render it inline, do nothing extra. `--dump` / `--dump-html` **only ever show names**
  (a dumped/exported file must stay portable — no bytes, no server), so download is
  irrelevant there; this whole feature is about the TUI and the *served* `--html`.

  **Per-type decisions** (schema from sessions `094539f2` + the image sample below):
  - **`file`** (×11) — **DOWNLOAD.** True user attachment; full bytes **inline** at
    `content.file.content`, path at `content.file.filePath` / `filename`. Embedded and not
    otherwise shown → highest-value download target.
  - **base64 images** — **DOWNLOAD** (TUI can't draw them) / **inline** in HTML via a
    `data:image/png;base64,…` URI. ⚠️ These are **NOT `attachment` events** — they arrive
    as image *content blocks*: `content[].type=="image"` with
    `source.{type:"base64",media_type:"image/png",data}`, and as tool results
    `toolUseResult.type=="image"` / `toolUseResult.file.base64` (`file.type=="image/png"`).
    So this spans a second code path (message/tool-result parsing), not just attachments.
    Confirmed present in `…/kwire/0877607A…/subagents/agent-ae0fffd8cb51d3c05.jsonl`
    (6 image blocks). Embedded → prefer download over reveal (more reliable than a path).
  - **`plan_file_reference`** (×3) — gray area; plan markdown **inline** at `planContent`,
    path at `planFilePath`. FIRST verify whether the plan is already surfaced inline
    elsewhere in the transcript (e.g. an ExitPlanMode message). If **shown** → do nothing.
    If **not shown** → **DOWNLOAD** the embedded `planContent` (embedded ⇒ more reliable
    than reveal). (Currently the viewer renders no attachment content, so absent inline
    surfacing → download.)
  - **`edited_text_file`** (×91) — **REVEAL IN FINDER.** Its inline `snippet` is
    truncated (~8 KB) so not a faithful download; the real file lives at `filename` →
    reveal it.
  - **`compact_file_reference`** (×24) — **REVEAL IN FINDER.** Path-ref only
    (`filename` + `displayPath`), nothing embedded.
  - **`queued_command`** — **nothing.** Already decoded and shown inline as a `❯` turn
    (see the queued-messages work); would double-show if surfaced again.
  - Everything else (tool/agent/skill listings, `task_reminder`, permission/date/hook
    deltas, plan-mode toggles) is harness bookkeeping — ignore.

  **Mechanics.**
  - **Served `--html`:** downloads stream from a new loopback endpoint (extend the
    `html_export.rs` server beside `/__reveal`; e.g. `/__attachment?id=…` with
    `Content-Disposition: attachment; filename=…`); reveal-in-Finder reuses `/__reveal`.
    Images can also render inline as `data:` URIs.
  - **TUI:** show the name inline. Two actions, each available by **mouse click AND
    keyboard** (it's a TUI — keyboard-first):
    - **Download** (embedded types: `file`, base64 images, `plan_file_reference` if not
      shown) → decode + write bytes to `~/Downloads`, announce the saved path on the
      status line.
    - **Reveal in Finder** (path-only types: `edited_text_file`, `compact_file_reference`)
      → `app::reveal_in_file_manager` (`open -R`).
    Mouse: the release handler already maps a click to a path via `view.click_at(row,col)`
    → `reveal_in_file_manager`; extend `click_at` (or a sibling) to also return a
    *downloadable* hit so a click on an embedded attachment saves it. (Mouse capture is
    already on and coexists with the TUI's own text selection — drag=select/copy,
    click=fold/reveal — so there is NO text-selection caveat.) Keyboard: act on the
    **focused block** (`]`/`[` already move focus) — bind **`d` = download**, **`r` =
    reveal** (both free today: only `Ctrl-d` and no `r` are taken). If the focused block
    has no such attachment, no-op with a brief status hint.
    - **Save SYNCHRONOUSLY, not on a background thread.** Payloads are bounded
      (screenshots a few MB; `file`/plan text smaller), so base64-decode + write is
      ~tens of ms — below a frame, imperceptible on a deliberate action. A thread would
      add an `mpsc` channel + loop draining + in-flight/error state + double-trigger
      guarding to an otherwise synchronous event loop, for no real benefit. Show
      `Saving… → Saved to <path>` and keep the logic in a standalone
      `fn save_attachment(&Attachment) -> io::Result<PathBuf>` so it's a one-line switch
      to `thread::spawn` + channel IF we ever surface a genuinely large `file` payload.
  - **`--dump` / `--dump-html`:** names only.

  Model: a `Block::Attachment { kind, name, path: Option<String>, inline: Option<Vec<u8>>|String }`
  where `inline.is_some()` ⇒ downloadable, else reveal-by-path — plus the separate
  image-content-block path. *(Queued 2026-07-25; base64/image case confirmed 2026-07-25.
  Do not start until the current queued-messages changes are reviewed.)*

- [~] **Track sub-agent activity (spawned from the main session).** *(TUI shipped;
  HTML drill-down remaining.)* Design in `design/subagents/`. **TUI complete** (stages
  1–6, verified end-to-end in tmux): the spawn renders as an agent-hue `⏺ Agent(type:
  description)` block (`bd1de8e`); descend/ascend via a View stack keeping ancestors
  alive (`8d5b088`); the footer's `esc back` / `active N` labels with fit-and-shed
  (`fddc…`); the `a` active-agents popup (`ec90c8e`); live-tail of an open child
  (`…`). Lifecycle discovered from real data: launched = spawn (`toolUseResult`),
  terminal = a completion `<task-notification>` keyed by tool-use-id/agentId (status ∈
  completed/failed/killed/stopped), work = the child `subagents/agent-<id>.jsonl`; no
  mid-run inter-agent events exist. **Remaining: the HTML export drill-down (§4)** — node
  sections, `↓ Children`, node-scoped filter/usage, `⧉` new-tab, hash routing. `--dump`/
  `--dump-html` stay unchanged by design. Below is the original scope for that stage.

  **Discovery — VERIFIED (2026-07-25, sub-agent study of session `094539f2` + kwire).**
  Sub-agent turns are **NOT inlined** in the parent (`isSidechain` is `false` on *every*
  parent record) — they live in separate child files:
  ```
  <projectDir>/<sessionId>.jsonl                              ← parent transcript
  <projectDir>/<sessionId>/subagents/agent-<agentId>.jsonl    ← child transcript
  <projectDir>/<sessionId>/subagents/agent-<agentId>.meta.json ← {agentType,description,toolUseId,spawnDepth}
  ```
  - **Spawn record (parent):** a `tool_use` named **`Agent`** (NOT `Task` in current
    versions — match both). `input`: `description`, `prompt`, `subagent_type`,
    `run_in_background`. Its `id` is the `toolUseId`.
  - **Join key (gold):** the matching `tool_result` record carries top-level
    **`toolUseResult.agentId`** → child file is `subagents/agent-<agentId>.jsonl`.
    `agentId` and `toolUseId` are **independent random ids** — never string-transform;
    join via `toolUseResult.agentId`, or `meta.json.toolUseId == Agent tool_use id` for
    in-flight agents with no result yet. The pairing is exact 1:1, so **parallel** Agent
    calls disambiguate cleanly (do NOT rely on order/timestamp/sessionId).
  - **Child shape:** same schema as a normal session (parses via existing `parse_main`);
    every record has `isSidechain:true` + an `agentId` field (= file stem); it **shares
    the parent's `sessionId`** (so sessionId alone can't tell parent from child); root
    `parentUuid` is `null` (independent uuid chain). Sanity check: child's first `user`
    message == parent Agent tool_use `input.prompt` (byte-equal).
  - **Result:** async spawns (`status:"async_launched"`) put the final answer out-of-band
    at `toolUseResult.outputFile`; sync spawns (`status:"completed"`) inline it in
    `toolUseResult.content` (+ token/duration stats). Handle both.
  - **Not spawn records:** `attachment.type=="agent_listing_delta"` is the available-agent
    roster UI feed — exclude. `tool-results/` and the `.output` file are overflow/answer
    artifacts, not transcripts.
  - **Unconfirmed:** nested sub-agents (`spawnDepth ≥ 2`) weren't present — grandchild
    placement (same `subagents/` dir vs. nested) is unknown. Codex sub-agents unstudied.

  **Model:** for each `Agent` spawn, read the child file at `agent-<agentId>.jsonl`, parse
  it via `parse_main` into its own `Vec<Block>`, and attach to the parent — a dedicated
  `Block::SubAgent { agent_type, description, blocks, result }` (or a child field on
  `Block::ToolUse`). Discovery needs the parent path → derive `subagentsDir`; needs
  filesystem access (fine for the TUI/`--html`; a shared `--dump-html` won't have the
  child files, so it degrades to the parent's summary only).

  **Render:** a collapsible drill-down under the `Agent` call — collapsed = agent label +
  summary (as today); expanded = the child's turns indented as a sub-transcript. TUI fold
  state, `--dump`, `--html` alike.

  **Live tail:** children are separate files that grow independently — the tailer must
  also follow open child transcripts (by `agentId`), not just route by uuid within one
  file.

  Credited to **claude-code-trace**'s "subagent drill-down" idea (see Attribution). The
  discovery mechanism is nailed down above; remaining work is the model + render + tail.
  *(Queued 2026-07-25; discovery verified 2026-07-25.)*

- [x] **TUI Edit/Update diff line numbers render wrong.** Reported 2026-07-25; **resolved**
  (verified 2026-07-26). `render::render_patch` and `html_export::diff_part` now number
  identically — both advance the old-side counter `o` on context lines. Verified with a
  `structuredPatch` Edit (`--dump` vs `--dump-html`): both emit `10,11,12(del),12(add),13`.
  The two numberers were folded into one in the engine refactor (M13, done).

- [x] **Unify the parse backend + make it a reusable engine.** *(Done — engine refactor
  M1–M16; see `design/engine-refactor-plan.md`.)* Both surfaces now share ONE backend: the
  `claude-replay-core` crate yields `parse_session(path) -> Session { blocks, index,
  metrics, cwd, … }`, with the TUI (`render.rs`) and HTML (`html_export.rs`) reduced to
  formatters over it. The core is an independent library (no TUI/HTML/CLI deps) that any
  program can use to parse Claude/Codex transcripts into the block model. **Remaining
  sub-item:** a documented third-party usage example (`claude-replay-core/examples/`) —
  tracked under the API-ergonomics review.

- [ ] **Download transcripts from web / desktop sources.** *(Queued 2026-07-25;
  research in `design/transcript-sources.md`.)* Today the viewer reads local `.jsonl`
  (Claude Code CLI `~/.claude/projects`, Codex CLI). Investigate pulling transcripts from:
  Claude **web** (claude.ai — ref: simonw's `claude-code-transcripts`), Claude **Design**,
  Claude Code / **cowork** in the **desktop app**, and **Codex web**. For each: is there
  an export/download path (official API, share-link JSON, local desktop store, or DOM
  scrape), what auth it needs, the on-disk/wire format, and how it maps to our block
  model. Feasibility + a recommended acquisition path per source; build only after.

- [ ] **HTML sidebar: clicking a turn highlights the previous turn.** Reported
  2026-07-25 (live `-f --html`, likely all HTML). Clicking a turn in the left pane
  scrolls the right pane to that turn's user message correctly, but the left-pane active
  highlight stays on the *preceding* turn, and a second click is a no-op (scroll already
  there, so no `scroll` event → `spy()` never re-runs). Cause is in `export.js`: `goTo`
  scrolls with `- GOTO_Y (120)` offset while `spy()` marks active the last turn whose top
  is `<= STICKY_Y (72)` — so the clicked turn lands just below the sticky line and its
  predecessor is still the last one above it. Fix: after `goTo`, set the active turn to
  the *clicked* target directly (don't wait for `spy()`), or reconcile `GOTO_Y`/`STICKY_Y`
  so the landed turn is the one `spy()` selects.

- [x] **Study: can a live session be identified, located, and typed into?** ✅ answered —
  full findings in **`design/session-liveness-probe.md`**; instrument in
  **`examples/session_probe.rs`** (an example, deliberately never wired into the CLI).
  Prerequisite knowledge for the machine-wide monitor (`#98`). Headlines:
  - **Liveness** needs exactly two signals — a live process (basename of **argv[0]**, see
    the traps below) and the **tree** mtime (root + `subagents/`); the root mtime alone is
    redundant. The in-flight-tool check stays for the case mtime cannot cover: an agent
    blocked in a long tool call writes nothing anywhere while being maximally busy.
  - **Two silent process-matching traps.** Matching argv *anywhere* makes every agent tool
    shell look like an agent. Matching `comm` from a **bulk** `ps` listing is worse — the
    multi-column form **truncates** it (`/Users/hong/.local/bin/claude` → `/Users/hong/.loc`),
    dropping every absolute-path agent. jdi is unaffected (it reads `ps -o comm= -p <pid>`
    per pid, which does not truncate).
  - **Process → session is the real gap.** `--resume` carries the id only for *resumed*
    sessions (7 of 11 live agents had none). Fallbacks: an open `.jsonl` fd works for Codex
    but not Claude; cwd → project-slug works for Claude. Neither is exact, so cross-check the
    transcript's recorded `session_id`.
  - **Injection is a property of the multiplexer**, not the terminal or the OS: tmux
    `send-keys` ✅, screen `-X stuff` ✅ (needs a *literal* newline), external `TIOCSTI`
    ❌ `EPERM`, tty write reaches the display but never stdin. **Careful:** `TIOCSTI` on
    one's *own* stdin is accepted on macOS, so a naive probe reads as a green light.
  - **Safety gate (§4 of the note).** Injection executes instructions as that user in a
    session holding their tools and credentials. Any productisation needs per-session
    consent at the time, visibility in the target, local-only, refuse-by-default. `#98`'s
    read-only monitor needs none of this; anything that types does.

- [x] **Durable, cross-run session cache (`#96`).** ✅ shipped v1.35.0 — design of record in
  **`design/durable-session-cache.md`** (v27, with §14 recording where the build chose
  differently). A second run resumes the fold instead of re-reading the transcript: on real
  sessions here, 99.99% of a 107 MB file skipped, block-identically. Shape:
  - **Two append-only streams** — the frontend's own `BlockStore` backing is the content
    stream (nothing stored twice), plus one `MetaRecord` per committing drain.
  - **One principle.** A resume point is an `(offset, state)` pair such that folding from
    `offset` seeded with `state` yields exactly what a cold parse yields — so `replay_from`
    is a *partition*, not a bookmark, and the resumed fold suppresses nothing.
  - **The oracle is always a cold parse**, block for block: a corrupt-but-plausible resume
    passes every self-consistency check there is. Asserted for clean resumes,
    resumes-from-resumes, and every truncation of both streams.
  - **Two-outcome admission** (`Owned` / `Denied`) — an entry is never shared, so on denial
    nothing was opened; the cache-less fallback is a separate explicit call.
  - Retired: `Body::Hibernated` and `PersistentStore` (a frozen materialization that could
    never fold again, and a persisted render continuation §4.3 rules out — the continuation
    is now *derived* from the restored prefix).

- [x] **The durability frontier cannot freeze (`#96` follow-up).** ✅ shipped v1.36.0 — found
  by asking why the resume above stopped at 65% of one file. `last_skill` capped the drop at
  the last `Skill`'s turn, and that same cap stopped `base` from ever reaching it, so its
  clearing condition was unreachable: **one skill call froze the fold for the rest of the
  session** — 3218 of 12466 blocks (26%) resident, re-finalized and re-diffed every 250 ms
  tick, violating the O(open turn) invariant #74/#84/#85 all rest on.
  - **The skill pin is deleted**, because bounding what it protected removes its reason: a
    body may only nest into a `Skill` in the same turn. The data says that is what it always
    was — 27 of 32 bodies across real transcripts arrive two lines after their call, and
    every long-reach case is a `jdi-handoff` body injected with no `Skill` call at all. That
    also fixed the rendering it came from (the orphan used to glue itself onto an unrelated
    skill 126,000 lines back). `FOLD_VERSION` → 2.
  - **The queue pin is made precise, then bounded** — it capped at the oldest live marker's
    turn rather than blocking every commit, and lets go past `MAX_PINNED_TURNS`.
  - The byte gate does *not* reach this (its corpus has no orphaned bodies), so the guard is
    three tests, one of which measures the resident window on real transcripts — the failure
    is silent (a pinned session renders perfectly and simply stops committing).

- [ ] **`claude-monitor` — every session on the machine, over HTTP (`#98`).** Design **v4** in
  **`design/claude-monitor.md`** (2026-08-06, for review); NOT started. Unblocked by #96. Three
  review rounds made it smaller each time — both of v3's central proposals were rejected and
  replaced with *less*:
  - **The session title left the meta record.** Putting it there would have put I/O inside the
    **sans-io** fold (an agent's title may live in its own database, e.g. QoderWork), bumped
    `FOLD_VERSION` for every user to serve one consumer, and tied an occasional refresh to a
    per-commit cadence. It is now a `TranscriptAdapter::session_card(path)` hook — the same class
    as `load_tasks`, path-taking and never called by the fold — plus the monitor's own
    `cards.json` with its own refresh policy (new session · swept · turns advanced by N).
  - **That hook lands on its own, before the monitor.** Both shipped frontends show a UUID where
    a name belongs today (`app.rs` uses `path.file_stem()`; `display_title` falls back to the repo
    name), so this improves the product now and gets exercised by two frontends before a new
    application depends on it.
  - **The page's rail slot became nothing.** A slot is shaped like one host's layout; the next
    host needs a different one. The unit of reuse is a **URL** — the crate serves
    `/session?id=<sid>` and hosts compose at the document level (the monitor: a rail plus an
    iframe). Nothing host-specific enters the html crate, the page stays byte-identical, and
    swapping the frame's `src` keeps the rail's state, which resolves the page-reload question.
    §6.4 names what the URL boundary does NOT give (restyling, a shared scroll context) and what
    it would take to lift that later (a scoped component) rather than pretending it is free.
  - Still standing from v2/v3: its own cache root (removes the cold-index gap and all lock
    contention), growth-by-`stat` as the primary liveness signal, and `Live` → a public
    `SessionService` + `ServiceConfig` so `--html` and the monitor share one implementation.
  - Eight open questions in §13, plus a record of the five the review resolved and why.

- [ ] **The `session_card` memo (`#98` prerequisite).** Design in **`design/session-card.md`**
  (v1, for review); NOT started. Revises the interface shipped in v1.40.0, which derives a title
  correctly but pays full cost every time — **0.96 ms/session**, flat in transcript size (the tail
  is bounded) but linear in session count, so 1000 sessions is ~1 s per refresh spent almost
  entirely re-reading bytes that did not change. The floor is one `stat`: **1.3 µs**.
  - **The caller cannot fix this.** A framework `(len, mtime)` cache is *wrong for QoderWork*,
    whose title lives in SQLite and changes with the transcript untouched — it would pin such a
    title forever. Only the adapter knows what its answer depends on, so the staleness decision
    moves to the adapter, and it needs somewhere to keep what it learned: an **opaque JSON memo**
    the caller stores and hands back, never interprets.
  - **Three outcomes, not `Option`**: `Unchanged` / `Fresh` / `Absent`. With `Option`, "keep
    yours" and "nothing here" collapse into `None` — one makes titles vanish on the next poll, the
    other makes a deleted one linger.
  - **The memo must always be optional**: missing, foreign or stale-format is a cache miss and the
    cold path, never an error. That discardability is what distinguishes it from #96's rejected
    opaque payloads — a cache its owner may throw away, not a format readers depend on.
  - Also adds a provided `session_cards` batch (mirroring `subagent_sources`) because QoderWork's
    per-session cost is *opening the database*, which a memo cannot fix but a batch can.
  - Five open questions in §8.

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
