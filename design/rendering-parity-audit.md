# Rendering parity audit: what the classic page presents that the app shell does not (#105)

The monitor has two front ends for the same record stream: the classic page
(`claude-replay-html/src/html/export.js`, the reference) and the app shell
(`claude-monitor/src/codex-ui/`). How a transcript is presented is the product; the app shell
re-implemented presentation and, as with scrolling (#98, `design/virtual-window.md`), inherited
some of the classic page's decisions and not others. This audit inventories every presentation
feature and reading control of both pages from the code, marks each as ported, missing, or
app-only, recommends what to keep and what to forgo, and lays out the shared rendering modules
that end the fork. It is a **test matrix**: a row is closed by a scenario in
`claude-replay-browser-tests/tests/scenarios.rs` that runs on both surfaces, not by this document
(the rule from #98 — a rule with no case is re-derived incompletely by the next implementation).

Method: two independent code inventories (one per page, every rendering path and control read
with file and line), spot-verified against the code and, for the owner-named items, against the
browser. Line numbers are as of v1.176.0.

## What is already shared, and therefore identical

Most of the *content* rendering is server-side in `html_export/mod.rs` and rides the wire as
pre-rendered parts, so both pages show the same thing without either implementing it:

- Markdown → HTML (`md_html`, tables and strikethrough; `h3+` collapse to `h3`; raw HTML escaped;
  images become `[image] link`), including the **user-turn rule** that a single newline is a
  hard line break (`md_html_user`) while assistant prose soft-wraps.
- **Pasted terminal art detection** (`user_body_parts` / `preformatted_runs`): runs of lines that
  are mostly box-drawing, pure structure, JSON members or structural terminators are lifted out
  of markdown into a verbatim `raw` part. The app shell renders `raw` and `pre` parts as
  `<pre>` (`view-model.js partsHtml`), so **this is ported** — the shell needs no detector.
- Fenced code cards with a language label, a copy button and syntect highlighting; numbered
  source rows (`num`) and unified diff rows (`diff`) with real line numbers; tool chips
  (`N lines`, `+adds/−dels`, `exit N`, `failed`, durations); the fold policy's default `open`;
  attachments with their two capability stamps; runtime and usage facts.
- Reading preferences (code size, wrap, wide) share one key through `shared/reading.js`; the
  keyboard map is `shared/keymap.js`; capability decisions are `shared/capabilities.js`;
  runtime rows are `shared/runtime.js`; ids, labels, record stream, control protocol likewise.

The owner's two named checks, answered: **verbatim/markdown detection is ported** (server-side,
both pages render the same parts); **raw text of a user turn is not** — see row 1.4.

## The matrix

Verdicts: **KEEP** = the classic behaviour is right, port it · **HAVE** = ported already ·
**OK-DIFF** = both have it, the app shell's form is acceptable or better · **FORGO** = do not
port, with the reason · **APP-ONLY** = the app shell has it and the classic page does not.
Scenario: the state of the guard that runs on both surfaces.

### 1. User prompts

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 1.1 | Prompt markdown with hard line breaks | server `md_html_user` | same wire, same HTML | HAVE | none → add to the prompt scenario |
| 1.2 | Pasted terminal art shown verbatim | server detector → `pre.raw` (wrap toggle applies) | `raw` part → `<pre>` in `.body.markdown` | HAVE | none → add: a pasted box stays a box on both |
| 1.3 | Long prompt clamp | 12 line-heights + fade + `⋯ N more lines`, re-collapsible | 560 chars → 246 px + fade + "Show the whole prompt" | OK-DIFF | none → add |
| 1.4 | **Raw text of a user turn** | `{}` shows `src` — exactly as typed, whitespace intact; per turn (`.rawbtn`) and global (`#btn-raw`); wrap applies | was: the JSON record, per turn only. Now (#109): `{}` on a user turn shows `src` as typed (`pre.turn-raw-text`, wrap follows the reading preference), a per-turn override flips away from the global, and "User turns as raw text" sits with the reading controls — one preference with the classic page under the shared reading key, the classic page's old key folded in once; a global change clears per-turn overrides on both. Per-turn overrides across a reload stay with #114 | HAVE (v1.178.0) | ✓ both: `scenario_raw_text_of_a_user_turn` |
| 1.5 | Slash-command turn is a turn | foldable card with badge, arg preview, `N lines`, outputs; a sidebar row | `command` → a "system" renderer row inside the process; no turn, no pane row | **KEEP** (port) | none → add |
| 1.6 | Queued prompt marker | `⧗ queued:` line | "Queued input" renderer, never folded | HAVE | `scenario_queued_prompt_text` ✓ both |
| 1.7 | Prompt attachments (images, files) | attachment cards after the turn (`amark`) | cards under the prompt (`prompt-attachments`) with capability glyphs | OK-DIFF | image scenario ✓ both |
| 1.8 | Spot / deep link on a turn | `#` copies URL+`#id` | same | HAVE | none → add (with 3.11) |
| 1.9 | Copy the message text | none (select text) | none | — | owner's #99 (a selected one-line prompt pastes as three): fix there; a "copy message" control is the durable answer, propose with #99 |

### 2. Assistant text

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 2.1 | Markdown rendering | server | server, plus wide tables get a scroll box | HAVE (+) | none → add: a wide table scrolls on both |
| 2.2 | Fenced code card + copy | server card; `.cpy` copies the code | same card; copy with "Copied" | HAVE | none → add |
| 2.3 | Commentary vs final phase | stated `commentary` muted | commentary rows live inside the process as "Progress" rows; final answers get answer chrome | OK-DIFF | none → add: a mid-process progress line lands in the process on the app shell, muted on classic |
| 2.4 | Thinking fold with a summary line | `✻ thought for Xs` / activities summary; body in a rail | renderer with the summary as target; empty thinking is a non-interactive head | HAVE | none → add |
| 2.5 | Proposed plan | emitted (`presentation`) but not consumed | a "Proposed plan · review before implementation" card | APP-ONLY | none |
| 2.6 | Plan attachment inline viewer | `▸ view plan` inline | preview pane | OK-DIFF | none |

### 3. Tool calls and process

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 3.1 | **Output caps** (`⋯ N more lines · to line M`) on `pre`/`num`/`diff` parts | first 12 (pre, diff) / 10 (num) rows, the rest behind a button; small expansions remembered per record | was: the server's `cap` ignored, every row in the DOM behind a 360 px scroll box. Now `shared/parts.js` (#108): both pages run one split, label, row markup and memory; the classic page's memory inside a nested fold (every tool call sits in an activity fold) was never re-applied and is fixed by the same scenario | HAVE (v1.177.0) | ✓ both: `scenario_output_caps_expand_and_remember` |
| 3.2 | Line-number gutters unselectable | `.gut{user-select:none}` | `.ln` selectable — a copied block carries its numbers | **KEEP** (port, one rule) | none → add: selecting two rows copies two lines on both |
| 3.3 | Per-pane code bar: copy the pane (no gutters/marks), size, wrap | per `.numbered`/`.diff` pane | global reading controls only; no "copy this pane" | KEEP the copy; FORGO the per-pane size/wrap (global is enough) | none → add: copying a diff pane yields the lines without `+`/`−` on both |
| 3.4 | Tool head: name, target, chips, state | name/target/chips; failure chip red | name/target; chips fold into a state pill (failed/running/completed via regex on chip text) | HAVE, verify the `N lines` and duration chips reach the head | none → add: a failed Bash shows `failed` and its exit on both |
| 3.5 | Edit diff open by default, others closed | fold policy `open` | `rendererStartsClosed`: closed unless running / interaction / queue — an Edit diff starts closed | OK-DIFF (the app shell's process rows are compact by design; the reader opens a diff) | none → add |
| 3.6 | Diff rendering (unified, marks, tinted code column) | `.nrow.add/.del` | `.line.add/.del` with marks | HAVE | none → add |
| 3.7 | Numbered source (Read/Write) with highlighting | `.numbered` | `.codebox` | HAVE | none → add |
| 3.8 | File path links with reveal/render stamps; in-page viewer | `a.tool-path`; modal viewer | `.renderer-target-link`; preview pane; lightbox | HAVE | `the_app_shell_*` reveal cases ✓ (app), classic has `browser_follow` file cases; a both-surface scenario → add |
| 3.9 | Images | inline ≤520 px, lightbox | collapsed → thumbnail → lightbox (#80, deliberate) | OK-DIFF | ✓ both (#106 tightened) |
| 3.10 | Attachments (non-image) | `▤ kind name` card, download/reveal | `renderer-note` with capability button | HAVE | none → add |
| 3.11 | **Deep links to tool records** | `#b7` lands on any block, opening its fold chain | spot links and hash landing only for user/assistant units | **KEEP** (port) | none → add |
| 3.12 | Sub-agent spawn: badge, `N tools · launched`, open child | fold + `↵ child` + `⧉` new tab | "Agent event" + "Open child transcript"; parent button `u` | HAVE; FORGO `⧉` (a single-page app; the session list opens any session) | `the_app_shell_*` child cases ✓ (app); both-surface → add |
| 3.13 | Workflow fleet roster under the launching block | in-flow roster with running dots | agents pane (+ run members) | FORGO in-flow (the pane covers it) | — |
| 3.14 | Artifact link on the publishing tool's head | header target becomes the link | roster + `↳` jump (#78; moving to the right pane, #95) | OK-DIFF; verify the head link exists on the app shell | none → add with #95 |
| 3.15 | Compaction | hairline seam in flow + sidebar tick | tick in the turns pane (#86) + a folded "Context compacted" renderer in flow | HAVE | ✓ app (#86); classic tick case → add |
| 3.16 | MCP calls grouped in the filter | `MCP → server → tool` tree | flat tool names | FORGO for now (the flat list works until a session has dozens of MCP tools) | — |
| 3.17 | `request_user_input` card | ignored | waiting/resolved card with answers | APP-ONLY | ✓ app |
| 3.18 | Bare tool result | fold `Result` | generic renderer | HAVE | — |
| 3.19 | Timestamps on user turns | `h:mm` today, `Mon D` older (+ year) | none; the wire carries `ts` | **KEEP** (port) | none → add: a turn from yesterday shows its date on both |
| 3.20 | "Turn NN" labels | sticky bar `Turn N — label`; sidebar rows | pane rows; a label on process surfaces whose ordinal is wrong (#103) | #103 fixes the ordinal; FORGO the sticky turn bar (the pane's current-turn row covers it) | #103 |

### 4. Folding and disclosure

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 4.1 | Per-block fold, keyboard toggle (Space/Enter on a focused head) | yes | yes | HAVE | none → add |
| 4.2 | Expand all / collapse all folds | record-level, pinned as user overrides so re-emission cannot undo them | `#sessionFoldAll` (every process surface), per-process bulk, per-subtree bulk | OK-DIFF; verify re-emission keeps a user's open state on the app shell | none → add: open a fold, let the tail rewrite, still open on both |
| 4.3 | Progressive rows inside a long process | (folds only) | first 7 events, "Show N more" | APP-ONLY (keep) | ✓ app (#98 uses it); — |
| 4.4 | **Fold / raw / expansion state survives reload and session switch** | sessionStorage per session (folds, raw overrides, small `more` expansions, anchor, read count) | position only (`view-memory.js`); folds/raw/prompt/image sets cleared on switch | **KEEP** (port) | none → add: open a fold and raw a turn, reload, both hold |
| 4.5 | Navigation reveals context monotonically | `revealMark` opens caps/clamps holding a hit | `revealNavigationContext` opens process/progressive/renderer | HAVE; caps join it with 3.1 | #100 |

### 5. Navigation, search, reading aids

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 5.1 | Turn list + scroll-spy | sidebar, active row | turns pane, current row (#52) | HAVE | ✓ app; both → add |
| 5.2 | Turn stepping `]`/`[`, head stepping `j`/`k`, page Space | yes | yes | HAVE | none → add |
| 5.3 | Search haystack | the records' text | `JSON.stringify(record)` with tags stripped — matches keys and head metadata too | **KEEP** (port: search text, not JSON) | none → add: a query that is only a JSON key finds nothing on both |
| 5.4 | Hit highlighting | every occurrence marked, the current one stronger | only the current hit's block marked | **KEEP** (port) | none → add |
| 5.5 | Hit stepping with reveal | record-first order, wrap, on-screen hit highlighted in place | wraps; reveal misses caps (#100) | #100 | #100 |
| 5.6 | Scope filter with per-class counts; scope prefix `uatobrew:`; whole words | dropdown + typed prefix + counts | dropdown without counts, scope not honoured while stepping (#101) | #101 for counts and scope; FORGO the typed prefix; whole-words: KEEP (cheap, useful for short identifiers) | #101 |
| 5.7 | Search on large transcripts | incremental | incremental (sluggish above ~10 MB, #104) | #104 | #104 |
| 5.8 | **Type / tool filter** | non-matching records hidden, turns dimmed as landmarks, matching folds force-open, lands on the nearest hit, ✕ restores the fold snapshot | non-matching rows dimmed only | **KEEP** (port hide + force-open + land) | none → add |
| 5.9 | Filter-hit stepping ‹ › and `n`/`N` | yes | no (#94, owner-deferred) | #94 | — |
| 5.10 | Jump-to-bottom / new-messages pill | one pill | one pill (#64) | HAVE | ✓ both |
| 5.11 | Follow indicator `⤓ following live` | chip | the pill is a circle while following | FORGO (the pill states it) | — |
| 5.12 | Theme, code size, wrap, wide | yes (shared key) | yes (shared key) | HAVE | theme ✓ app; reading → add |
| 5.13 | Keyboard map | shared table; `\ o c u ↑↓` inert | every key wired | HAVE (+) | none → add: `w` wraps on both |
| 5.14 | Sidebar / outline collapse | — | `\`, `o`, three states | APP-ONLY | ✓ app |
| 5.15 | Global search overlay ⌘K | — | yes | APP-ONLY | — |
| 5.16 | Landing flash / hold after a jump | 1 s flash; 2 s hold | flash; 3-pass converge | HAVE | — |
| 5.17 | Upward drag-selection auto-scroll | custom | native (inner scroller) | FORGO | — |

### 6. Meta

| # | Feature | Classic page | App shell | Verdict | Scenario |
|---|---|---|---|---|---|
| 6.1 | Session id, path, copy | id strip + copy path | title menu with id and path (#83) | HAVE | ✓ both |
| 6.2 | Usage and runtime rows | side panels | info pane groups (#67/#68), shared `runtime.js` | HAVE | ✓ app |
| 6.3 | Parent / children | crumbs + `↑`; agents menu | parent button `u` (#82); agents pane | HAVE | ✓ app |
| 6.4 | Artifacts | menu, one row per URL | roster (#78) → right pane (#95) | HAVE | ✓ app |
| 6.5 | Tasks | floating panel, ⌖ centring | tasks pane, popover (#60), `c` centring (#57) | HAVE (+) | ✓ app |

## Keep, forgo, and why

**Keep and port (in this order — reading value per unit of work):**

1. **3.1 Output caps.** The single largest readability and performance gap: the classic page
   never puts more than a dozen rows of a tool result on screen unasked, and remembers what the
   reader opened; the app shell renders everything and scrolls it inside a box. The server
   already ships `cap`; the shell has to honour it. Do this as the first *shared* rendering
   module (below), so both pages run one implementation of caps and expansions.
2. **1.4 Raw text of a user turn.** The owner's "row text": the app shell's raw toggle shows the
   JSON record, which is a debugging view, not the reader's escape hatch from lossy markdown.
   The wire carries `src`; show it, per turn and globally, persisted.
3. **5.8 Filter hides.** Dimming leaves a 900-turn session as long as it was; hiding non-matching
   records with turns as landmarks is what makes "show me the Edits" usable.
4. **5.3 + 5.4 Search haystack and hit marks.** Searching JSON keys produces phantom hits;
   marking only the current block hides how many hits a screen holds.
5. **3.19 Timestamps.** A turn's time is on the wire and the app shell shows none.
6. **1.5 Command turns.** A `/command` is the user speaking; it belongs in the turns pane.
7. **4.4 State across reload.** Folds, raw and expansions the reader chose must survive a reload
   and a session switch, as position already does.
8. **3.2 + 3.3 Gutters and pane copy.** One CSS rule and one button; both about what ends up on
   the clipboard.
9. **3.11 Deep links to tool records.** Spot links on renderer heads; hash landing on any record.

**Forgo, and why:** the typed scope prefix (the dropdown is the same control), the sticky turn
bar (the pane's current row), the follow chip (the pill), the in-flow fleet roster (the agents
pane), `⧉` new-tab links (the session list), the MCP tree (premature), per-pane size/wrap
controls (global reading controls), per-kind keyline colours (the app shell has its own design
language; failure and running tones are what matter), the custom drag auto-scroll (native in an
inner scroller), inline-by-default images (#80 was a deliberate choice for screenshot-heavy
sessions).

**App-only features** (2.5 proposed plan card, 3.17 request-user-input card, 4.3 progressive
rows, 5.14/5.15 shell controls) stay; whether the classic page adopts the two cards is the
owner's call and not part of parity.

## Shared rendering modules

The owner's direction — factor out individual rendering modules shared across both shells — is
the right end state, and seam 0 (`design/monitor-shell-duplication.md`, `html/shared/`) is the
mechanism: one source, served as an ES module to the app shell and inlined into the classic page.
The constraint is markup: the two pages style different class names (`.nrow > .gut + .code`
versus `.codebox .line > .ln + .codecell`), and the classic page's rendered HTML is held
byte-identical by the gate. So a shared renderer takes a **class map** — the page passes its own
class names — and owns the *behaviour*; the classic page keeps producing byte-identical output
while running the shared code, which is what validates the module (the same pattern as #43).

| Module | Owns | Replaces | Task |
|---|---|---|---|
| `shared/parts.js` (landed, #108) | the row caps: split, label, `num`/`diff` row markup with a class map, the expansion memory keyed by record id + ordinal, the `pre` line rule (a trailing newline ends the last line) | classic `capped`/`numberedRows`/`diffRows` run on it, keeping their DOM and ordinal stamping; app `partsHtml`/`codeRows` render from it at render time with in-place expansion | 3.1 done; 3.2 (gutters), `md`/`note`/`blocks` markup and wrap classes remain per page for now |
| `shared/prompt.js` | the user turn's body: parts, the raw-text view from `src`, the long-prompt clamp | classic user card body + `rawFor`; app `renderUnit` user branch | 1.4, 1.3 |
| `shared/tool-head.js` | name/target/chips → state (failed/running/completed), exit and duration chips, the display-name rule | classic fold header pieces; app `viewRecord` chip regexes | 3.4 |
| `shared/search.js` | the haystack (record text), scope classes and per-scope counts, whole words, hit order (record-first), marks | classic `search`/`markHits`/`parseScope`; app `updateSearch`/`markSearch` | 5.3, 5.4, #101, #104 |
| `shared/filter.js` | type/tool filter semantics: hide vs landmark, force-open, nearest hit, snapshot restore | classic `setFilter`; app `applyFilters` | 5.8 |
| `shared/time.js` | `fmtTime` (`h:mm` today, `Mon D`, year when it differs), durations | classic `fmtTime`/`fmtDur`; app none | 3.19 |
| `shared/view-state.js` | folds, raw, expansions, prompt/image opens per session in `sessionStorage` (with position, `view-memory.js`) | classic `saveView`/`loadView`; app `view-memory.js` | 4.4 |
| `shared/virtual-window.js` | scrolling (#107, `design/virtual-window.md`) | | #107 |

Each module follows the shared-module conventions (no imports, one trailing `export` line, a
row in `SHARED`, the served list, the import-closure test) and lands with its scenarios on both
surfaces. The classic page adopting a module is not optional: it is the test that the module is
the classic behaviour.

## The test matrix

Every row above marked "none → add" becomes a scenario in `tests/scenarios.rs`, written ONCE and
run on both surfaces (the classic page as the reference), confirmed to fail on the old app-shell
code where it is a port, and named for its row. Rows marked ✓ name the scenario that already
holds them. A row that cannot be held by a scenario says why in the table. The follow-up tasks
below each carry their rows; a task is done when its rows are ✓ on both surfaces.

## Follow-up tasks (filed with this audit)

In priority order, all under "content rendering first": output caps as `shared/parts.js` ·
raw text of a user turn · filter hides · search haystack and marks · timestamps · command turns
· state across reload · gutters and pane copy · deep links to tool records · then the remaining
shared modules (`tool-head.js`, `search.js` with #101/#104, `filter.js`, `time.js`,
`view-state.js`) as their rows are ported. #103, #100, #101, #104, #99 stay as filed and are
referenced by their rows.
